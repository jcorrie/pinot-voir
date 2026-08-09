//! Blink external LED
#![no_std]
#![no_main]

use cyw43_pio::PioSpi;
use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::PIO0;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Duration, Ticker};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

enum LedState {
    Toggle,
}
static CHANNEL: Channel<ThreadModeRawMutex, LedState, 64> = Channel::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("led on!");
    let p = embassy_rp::init(Default::default());
    let mut led = Output::new(p.PIN_15, Level::High);

    let dt = 100 * 1_000_000;
    let k = 1.003;

    spawner.spawn(defmt::unwrap!(toggle_led(CHANNEL.sender(), Duration::from_nanos(dt))));
    spawner.spawn(defmt::unwrap!(toggle_led(
        CHANNEL.sender(),
        Duration::from_nanos((dt as f64 * k) as u64)
    )));

    loop {
        match CHANNEL.receive().await {
            LedState::Toggle => led.toggle(),
        }
    }
}

#[embassy_executor::task(pool_size = 2)]
async fn toggle_led(control: Sender<'static, ThreadModeRawMutex, LedState, 64>, delay: Duration) {
    let mut ticker = Ticker::every(delay);
    loop {
        control.send(LedState::Toggle).await;
        ticker.next().await;
    }
}
