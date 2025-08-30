#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use bytemuck;
use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_sync::mutex::Mutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, DMA_CH0, DMA_CH1, PIN_26, USB};
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_rp::{Peri, bind_interrupts};
use embassy_time::{Instant, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb_driver::EndpointError;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});
bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// Circular buffer for double buffering
struct CircularBuffer {
    buffer1: [u16; 128],
    buffer2: [u16; 128],
    current_buffer: bool,
    read_ready: bool,
}

impl CircularBuffer {
    fn new() -> Self {
        Self {
            buffer1: [0; 128],
            buffer2: [0; 128],
            current_buffer: false,
            read_ready: false,
        }
    }

    fn get_write_buffer(&mut self) -> &mut [u16] {
        if self.current_buffer {
            &mut self.buffer2
        } else {
            &mut self.buffer1
        }
    }

    fn get_read_buffer(&self) -> &[u16] {
        if self.current_buffer {
            &self.buffer1
        } else {
            &self.buffer2
        }
    }

    fn swap_buffers(&mut self) {
        self.current_buffer = !self.current_buffer;
        self.read_ready = true;
    }

    fn consume_read_buffer(&mut self) {
        self.read_ready = false;
    }

    fn has_data(&self) -> bool {
        self.read_ready
    }
}

static AUDIO_BUFFER: Mutex<ThreadModeRawMutex, CircularBuffer> = Mutex::new(CircularBuffer {
    buffer1: [0; 128],
    buffer2: [0; 128],
    current_buffer: false,
    read_ready: false,
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // USB CDC ACM setup
    static STATE: StaticCell<State> = StaticCell::new();
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let driver = Driver::new(p.USB, Irqs);

    let mut usb_builder = embassy_usb::Builder::new(
        driver,
        {
            let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
            config.manufacturer = Some("Embassy");
            config.product = Some("ADC Audio Stream");
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

    let mut cdc = CdcAcmClass::new(&mut usb_builder, STATE.init(State::new()), 64);
    let usb = usb_builder.build();

    spawner.spawn(usb_task(usb)).unwrap();

    // Spawn ADC task
    spawner.spawn(adc_task(p.ADC, p.DMA_CH0, p.PIN_26)).unwrap();

    // Main USB transmission loop
    usb_transmit_task(cdc).await;
}

#[embassy_executor::task]
async fn adc_task(
    adc_peripheral: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, PIN_26>,
) {
    let mut adc = Adc::new(adc_peripheral, IrqsADC, Config::default());
    let mut p26 = Channel::new_pin(pin, Pull::None);
    let mut dma = dma;
    const SAMPLE_RATE_HZ: u32 = 8000;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    info!("ADC task started, sample rate: {} Hz", SAMPLE_RATE_HZ);

    loop {
    let mut guard = AUDIO_BUFFER.lock().await;
    let buffer = guard.get_write_buffer();

        match adc
            .read_many(&mut p26, buffer, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                guard.swap_buffers();
            }
            Err(_) => {
                info!("ADC read error");
                Timer::after_millis(1).await;
            }
        }
    }
}

async fn usb_transmit_task(mut cdc: CdcAcmClass<'static, Driver<'static, USB>>) {
    let mut stats_timer = Instant::now();
    let mut total_bytes_sent = 0u32;
    let mut total_bytes_dropped = 0u32;

    loop {
        cdc.wait_connection().await;
        info!("USB connected");

        total_bytes_sent = 0;
        total_bytes_dropped = 0;
        stats_timer = Instant::now();

        loop {
            // Wait for data to be available
            // Wait for data to be available
            let mut audio_bytes_buf = [0u8; 256]; // Adjust size as needed
            let mut audio_len = 0;
            loop {
                let guard = AUDIO_BUFFER.lock().await;
                if guard.has_data() {
                    let audio_data = guard.get_read_buffer();
                    let bytes = bytemuck::cast_slice(audio_data);
                    audio_len = bytes.len().min(audio_bytes_buf.len());
                    audio_bytes_buf[..audio_len].copy_from_slice(&bytes[..audio_len]);
                    drop(guard);
                    break;
                }
                drop(guard);
                Timer::after_micros(100).await;
            }

            // Try to send data in smaller chunks to avoid blocking
            let mut sent = 0;
            let chunk_size = 64; // USB packet size
            while sent < audio_len {
                let end = (sent + chunk_size).min(audio_len);
                let chunk = &audio_bytes_buf[sent..end];
                match cdc.write_packet(chunk).await {
                    Ok(_) => {
                        sent = end;
                        total_bytes_sent += chunk.len() as u32;
                    }
                    Err(_) => {
                        info!("USB write error");
                        total_bytes_dropped += (audio_len - sent) as u32;
                        break;
                    }
                }
            }

            // ...existing code for chunked send loop above now handles all transmission...
        }
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) -> ! {
    usb.run().await
}
