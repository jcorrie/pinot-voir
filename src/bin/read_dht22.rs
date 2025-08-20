#![no_std]
#![no_main]

use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use embassy_dht::dht22::DHT22;
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Output, Pin};
use embassy_rp::peripherals::{DMA_CH0, PIO0};

use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_time::{Delay, Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>,
) -> ! {
    runner.run().await
}

// static SENSOR_PIN: Peri<'static, impl Pin>;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let delay = Duration::from_secs(1);
    let mut dht_pin = DHT22::new(p.PIN_16, Delay);

    loop {
        let dht_reading = dht_pin.read().unwrap();
        let (temp, humi) = (dht_reading.get_temp(), dht_reading.get_hum());
        defmt::info!("Temp = {}, Humi = {}\n", temp, humi);
        Timer::after(delay).await;
    }
}
