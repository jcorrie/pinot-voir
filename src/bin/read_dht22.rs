#![no_std]
#![no_main]

use embassy_dht::dht22::DHT22;
use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

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
