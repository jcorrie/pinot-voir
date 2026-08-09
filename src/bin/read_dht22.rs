#![no_std]
#![no_main]

use cyw43_pio::PioSpi;
use embassy_dht::dht22::DHT22;

use embassy_executor::Spawner;
use embassy_rp::gpio::Output;
use embassy_rp::peripherals::PIO0;

use embassy_time::{Delay, Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

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
