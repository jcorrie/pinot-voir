#![no_std]
#![no_main]

use cyw43_pio::PioSpi;
use embassy_dht::dht20::DHT20;

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Output;
use embassy_rp::peripherals::{DMA_CH0, PIO0};

use embassy_rp::i2c::{self, Config, InterruptHandler};
use embassy_time::{Delay, Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

use embassy_rp::peripherals::I2C1;
use embedded_hal_async::i2c::I2c;
use {defmt_rtt as _, panic_probe as _};

// bind_interrupts!(struct Irqs {
//     PIO0_IRQ_0 => InterruptHandler<PIO0>;
// });

bind_interrupts!(struct Irqs {
    I2C1_IRQ => InterruptHandler<I2C1>;
});

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let delay = Duration::from_secs(1);
    let sda = p.PIN_2;
    let scl = p.PIN_3;
    let mut i2c = i2c::I2c::new_async(p.I2C1, scl, sda, Irqs, Config::default());

    let mut dht_pin = DHT20::new(i2c, Delay);

    loop {
        let dht_reading = dht_pin.read().unwrap();
        let (temp, humi) = (dht_reading.get_temp(), dht_reading.get_hum());
        defmt::info!("Temp = {}, Humi = {}\n", temp, humi);
        Timer::after(delay).await;
    }
}
