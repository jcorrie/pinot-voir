#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use defmt::*;
use embassy_executor::Executor;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::gpio::Pull;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{ADC, CORE1, DMA_CH0, DMA_CH1, PIN_26, USB};
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_rp::{Peri, bind_interrupts};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::{Instant, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// Core stacks and executors
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// Audio data channel between cores
const AUDIO_BUFFER_SIZE: usize = 512;
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

// Interrupt bindings
bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});
bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// Audio data block
#[derive(Clone, Copy)]
struct AudioBlock {
    samples: [u16; AUDIO_BUFFER_SIZE],
    block_id: u32,
    timestamp: u64,
}

impl AudioBlock {
    fn new() -> Self {
        Self {
            samples: [0; AUDIO_BUFFER_SIZE],
            block_id: 0,
            timestamp: 0,
        }
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // Spawn Core 1 for ADC sampling
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                unwrap!(spawner.spawn(adc_task(p.ADC, p.DMA_CH0, p.PIN_26)));
            });
        },
    );

    // Core 0 handles USB
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        unwrap!(spawner.spawn(usb_task(p.USB)));
        unwrap!(spawner.spawn(usb_transmit_task()));
    });
}

// Core 1 - ADC sampling task
#[embassy_executor::task]
async fn adc_task(
    adc_peripheral: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, PIN_26>,
) {
    info!("ADC task starting on Core 1");

    let mut adc = Adc::new(adc_peripheral, IrqsADC, Config::default());
    let mut p26 = Channel::new_pin(pin, Pull::None);

    const SAMPLE_RATE_HZ: u32 = 8000; // Start conservative
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;
    let mut dma = dma;
    let mut block_counter = 0u32;

    loop {
        let mut audio_block = AudioBlock::new();

        // Capture samples via DMA
        match adc
            .read_many(&mut p26, &mut audio_block.samples, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                block_counter += 1;
                audio_block.block_id = block_counter;
                audio_block.timestamp = embassy_time::Instant::now().as_micros();

                // Send to Core 0 for USB transmission
                // This will block if Core 0 can't keep up, providing natural flow control
                AUDIO_CHANNEL.send(audio_block).await;

                if block_counter % 100 == 0 {
                    info!("ADC: Captured block {}", block_counter);
                }
            }
            Err(_) => {
                error!("ADC read error");
                Timer::after_millis(1).await;
            }
        }
    }
}

// Core 0 - USB device task
#[embassy_executor::task]
async fn usb_task(usb_peripheral: Peri<'static, USB>) -> ! {
    info!("USB task starting on Core 0");

    // USB setup
    static STATE: StaticCell<State> = StaticCell::new();
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let driver = Driver::new(usb_peripheral, Irqs);
    let mut usb_builder = embassy_usb::Builder::new(
        driver,
        {
            let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
            config.manufacturer = Some("Embassy");
            config.product = Some("Dual-Core ADC Stream");
            config.serial_number = Some("12345678");
            config.max_power = 100;
            config.max_packet_size_0 = 64;
            config
        },
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    let mut usb = usb_builder.build();
    usb.run().await
}

// Core 0 - USB transmission task
#[embassy_executor::task]
async fn usb_transmit_task() {
    info!("USB transmit task starting on Core 0");

    // Get CDC class instance (this is simplified - you'd need to properly share this)
    // In practice, you'd need to structure this differently to share the CDC class
    Timer::after_millis(1000).await; // Wait for USB to initialize

    let mut stats_timer = Instant::now();
    let mut blocks_transmitted = 0u32;
    let mut blocks_dropped = 0u32;

    loop {
        // Receive audio block from Core 1
        let audio_block = AUDIO_CHANNEL.receive().await;

        // Convert to bytes for transmission
        let audio_bytes = unsafe {
            core::slice::from_raw_parts(
                audio_block.samples.as_ptr() as *const u8,
                audio_block.samples.len() * 2,
            )
        };

        // Here you would transmit via USB CDC
        // For now, just simulate processing
        blocks_transmitted += 1;

        // Print statistics
        if stats_timer.elapsed().as_secs() >= 2 {
            let total = blocks_transmitted + blocks_dropped;
            let success_rate = if total > 0 {
                (blocks_transmitted as f32 / total as f32) * 100.0
            } else {
                100.0
            };

            info!(
                "USB Stats: {} transmitted, {} dropped ({}% success)",
                blocks_transmitted, blocks_dropped, success_rate
            );

            stats_timer = Instant::now();
        }

        // Small delay to simulate USB transmission time
        Timer::after_micros(100).await;
    }
}
