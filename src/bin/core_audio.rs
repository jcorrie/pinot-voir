#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use bytemuck;
use defmt::*;
use embassy_executor::{Executor, Spawner};
use embassy_rp::Peri;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Pull;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{ADC, CORE1, DMA_CH0, DMA_CH1, PIN_26, USB};
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::{Instant, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// ---------- Interrupts ----------
bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});
bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// ---------- Executors / Core stacks ----------
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// ---------- Audio channel between cores ----------
const AUDIO_BUFFER_SIZE: usize = 512;
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

// ---------- USB/CDC statics ----------
static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; MAX_USB_BUF]> = StaticCell::new();
static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
static CDC_CLASS: StaticCell<CdcAcmClass<'static, Driver<'static, USB>>> = StaticCell::new();
const MAX_USB_BUF: usize = 64;

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

    fn centre_samples(&self) -> [i16; AUDIO_BUFFER_SIZE] {
        self.samples.map(|x| (x as i16) - 2048)
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // ---------- Core1: ADC sampling ----------
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

    // ---------- Core0: USB + CDC ----------
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        // Build USB device + CDC class
        let driver = Driver::new(p.USB, Irqs);

        let mut usb_builder = embassy_usb::Builder::new(
            driver,
            {
                let mut cfg = embassy_usb::Config::new(0xc0de, 0xcafe);
                cfg.manufacturer = Some("Embassy");
                cfg.product = Some("Dual-Core ADC Stream");
                cfg.serial_number = Some("12345678");
                cfg.max_power = 100;
                cfg.max_packet_size_0 = MAX_USB_BUF as u8;
                cfg
            },
            CONFIG_DESCRIPTOR.init([0; 256]),
            BOS_DESCRIPTOR.init([0; 256]),
            &mut [],
            CONTROL_BUF.init([0; MAX_USB_BUF]),
        );

        let cdc = CDC_CLASS.init(CdcAcmClass::new(
            &mut usb_builder,
            CDC_STATE.init(CdcState::new()),
            MAX_USB_BUF as u16,
        ));

        let usb = usb_builder.build();

        // Run USB device + CDC TX task
        unwrap!(spawner.spawn(usb_device_task(usb)));
        unwrap!(spawner.spawn(cdc_tx_task(cdc)));
    });
}

// ---------- Core1: ADC sampling ----------
#[embassy_executor::task]
async fn adc_task(
    adc_peripheral: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, PIN_26>,
) {
    info!("ADC task starting on Core 1");

    let mut adc = Adc::new(adc_peripheral, IrqsADC, Config::default());
    let mut p26 = Channel::new_pin(pin, Pull::None);

    const SAMPLE_RATE_HZ: u32 = 44100;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    let mut dma = dma;
    let mut block_counter = 0u32;

    loop {
        let mut audio_block = AudioBlock::new();

        match adc
            .read_many(&mut p26, &mut audio_block.samples, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                block_counter += 1;
                audio_block.block_id = block_counter;
                audio_block.timestamp = Instant::now().as_micros();
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

// ---------- Core0: USB device run loop ----------
#[embassy_executor::task]
async fn usb_device_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) -> ! {
    info!("USB device task running");
    usb.run().await
}

// ---------- Core0: CDC TX task (owns the CDC class) ----------
#[embassy_executor::task]
async fn cdc_tx_task(cdc: &'static mut CdcAcmClass<'static, Driver<'static, USB>>) {
    info!("CDC TX task starting");

    let mut stats_timer = Instant::now();
    let mut blocks_ok = 0u32;
    let mut blocks_err = 0u32;

    loop {
        // Ensure host connected before we start draining
        cdc.wait_connection().await;

        // Drain audio blocks while connected
        loop {
            let block: AudioBlock = AUDIO_CHANNEL.receive().await;
            block.centre_samples();
            block.centre_samples();
            block.centre_samples();
            block.centre_samples();
            let bytes: &[u8] = bytemuck::cast_slice(&block.samples);

            if let Err(e) = write_cdc_chunked(cdc, bytes).await {
                warn!("CDC write error: {:?}", e);
                blocks_err += 1;
                // Break to re-sync connection if it dropped / stalled
                break;
            } else {
                blocks_ok += 1;
            }

            if stats_timer.elapsed().as_secs() >= 2 {
                let total = blocks_ok + blocks_err;
                let pct = if total == 0 {
                    100.0
                } else {
                    (blocks_ok as f32 / total as f32) * 100.0
                };
                info!(
                    "USB Stats: {} ok, {} err ({}% ok)",
                    blocks_ok, blocks_err, pct
                );
                stats_timer = Instant::now();
            }
        }
    }
}

// ---------- Helpers ----------
async fn write_cdc_chunked(
    cdc: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    data: &[u8],
) -> Result<(), embassy_usb_driver::EndpointError> {
    // CDC full-speed EPs are typically 64 bytes
    let max_packet = 64usize;
    let mut offset = 0usize;

    while offset < data.len() {
        let end = core::cmp::min(offset + max_packet, data.len());
        let chunk = &data[offset..end];

        // Ensure host is still connected
        cdc.wait_connection().await;

        // Write one packet
        cdc.write_packet(chunk).await?;
        offset = end;
    }
    Ok(())
}
