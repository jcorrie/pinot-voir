//! This example shows how to use USB (Universal Serial Bus) in the RP2040 chip.
//!
//! This creates the possibility to send log::info/warn/error/debug! to USB serial port.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
use bytemuck;
use core::fmt::{Error, Write};
use core::str::from_utf8;
use cyw43::Control;
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::Peri;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::USB;
use embassy_rp::peripherals::{ADC, DMA_CH1, PIN_26};
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Timer;
use embassy_time::{Delay, Duration};
use heapless::String;
use picoserve::extract::Json;
use picoserve::extract::State;
use picoserve::{
    AppRouter, AppWithStateBuilder,
    response::DebugValue,
    routing::{PathRouter, get, parse_path_segment},
};

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(
    /// Binds the ADC interrupts.
    struct IrqsADC {
        ADC_IRQ_FIFO => ADCInterruptHandler;
    }


);

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});

#[embassy_executor::task]
async fn logger_task(driver: Driver<'static, USB>) {
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);
    spawner.spawn(logger_task(driver)).unwrap();

    let mut counter = 0;
    const NUM_CHANNELS: usize = 1;
    const MAX_UDP_PAYLOAD: usize = 1024;
    const BUFFER_SIZE: usize = 1024;
    const SAMPLE_RATE_HZ: u32 = 44100;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    let mut adc = Adc::new(p.ADC, IrqsADC, Config::default());
    let mut dma = p.DMA_CH0; // We need this to be mutable
    let mut p26 = Channel::new_pin(p.PIN_26, Pull::None);
    let sample_frequency_s: u64 = 44100000;
    let port = 1234;
    let broadcast_addr = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), port);
    loop {
        counter += 1;
        log::info!("Tick {}", counter);

        let mut audio_buffer: [u16; BUFFER_SIZE] = [0_u16; BUFFER_SIZE];
        match adc
            .read_many(&mut p26, &mut audio_buffer, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                let audio_bytes: &[u8] = bytemuck::cast_slice(&audio_buffer);

                // Send data in chunks if it's too large for a single UDP packet
                for chunk in audio_bytes.chunks(MAX_UDP_PAYLOAD) {
                    log::info!("Sending UDP packet: {:?}", chunk);

                    // Small delay between chunks to avoid overwhelming the network
                    Timer::after_micros(100).await;
                }
            }
            Err(e) => {
                info!("ADC read error: {:?}", e);
                Timer::after_millis(10).await;
            }
        }

        Timer::after_secs(1).await;
    }
}
