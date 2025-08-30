#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use bytemuck;
use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, DMA_CH0, PIN_26, USB};
use embassy_rp::uart::Error;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_time::Timer;
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

async fn write_cdc_chunked<'a>(
    cdc: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    data: &[u8],
) -> Result<(), EndpointError> {
    let max_packet_size = 64;
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + max_packet_size).min(data.len());
        let chunk = &data[offset..end];
        // Wait for connection just in case
        cdc.wait_connection().await;
        // Try writing
        match cdc.write_packet(chunk).await {
            Ok(_) => offset = end,
            Err(e) => {
                // Handle or retry error (e.g., BufferOverflow)
                // Could add delay before retry, or return error to caller
                return Err(e);
            }
        }
    }
    Ok(())
}

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

    // ADC setup
    let mut adc = Adc::new(p.ADC, IrqsADC, Config::default());
    let mut dma = p.DMA_CH0;
    let mut p26 = Channel::new_pin(p.PIN_26, Pull::None);

    const BUFFER_SIZE: usize = 1024;
    const SAMPLE_RATE_HZ: u32 = 44100;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    loop {
        cdc.wait_connection().await;
        loop {
            let mut audio_buffer: [u16; BUFFER_SIZE] = [0_u16; BUFFER_SIZE];
            if let Ok(_) = adc
                .read_many(&mut p26, &mut audio_buffer, ADC_DIV, dma.reborrow())
                .await
            {
                let audio_bytes: &[u8] = bytemuck::cast_slice(&audio_buffer);
                info!("{}", &audio_bytes);
                // Write audio bytes to USB CDC ACM
                let result = write_cdc_chunked(&mut cdc, audio_bytes).await;
                match result {
                    Ok(_) => {}
                    Err(e) => {
                        info!("USB write error: {:?}", e);
                        break; // If USB write fails, break and wait for next connection
                    }
                }
            } else {
                break; // If ADC fails, break and wait for next connection
            }
            Timer::after_millis(5).await; // Throttle if needed
        }
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) -> ! {
    usb.run().await
}
