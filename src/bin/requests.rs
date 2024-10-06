//! This example uses the RP Pico W board Wifi chip (cyw43).
//! Connects to Wifi network and makes a web request to get the current time.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]

use core::str::from_utf8;

use cyw43_pio::PioSpi;
use defmt::{error, info, unwrap};
use embassy_dht::dht22::DHT22;
use embassy_executor::Spawner;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_time::{Delay, Duration, Timer};
use rand::RngCore;
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};
use static_cell::StaticCell;
use pinot_voir::common::dht22_tools::sensor_reading_to_string;
use pinot_voir::common::shared_functions::{
    blink_n_times, get_api_key_as_bearer_string, parse_env_variables, EnvironmentVariables,
};

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

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: EnvironmentVariables = parse_env_variables();
    info!("Hello World!");

    let p = embassy_rp::init(Default::default());

    let fw = include_bytes!("../../cyw43-firmware/43439A0.bin");
    let clm = include_bytes!("../../cyw43-firmware/43439A0_clm.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        p.DMA_CH0,
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
    unwrap!(spawner.spawn(cyw43_task(runner)));

    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    let config = Config::dhcpv4(Default::default());

    let mut rng = RoscRng;
    // Generate random seed
    let seed = rng.next_u64();

    // Init network stack
    static STACK: StaticCell<Stack<cyw43::NetDriver<'static>>> = StaticCell::new();
    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
    let stack = &*STACK.init(Stack::new(
        net_device,
        config,
        RESOURCES.init(StackResources::new()),
        seed,
    ));

    unwrap!(spawner.spawn(net_task(stack)));

    loop {
        match control
            .join_wpa2(
                environment_variables.wifi_ssid,
                environment_variables.wifi_password,
            )
            .await
        {
            Ok(_) => break,
            Err(err) => {
                info!("join failed with status={}", err.status);
            }
        }
    }

    // Wait for DHCP, not necessary when using static IP
    info!("waiting for DHCP...");
    while !stack.is_config_up() {
        Timer::after_millis(100).await;
    }
    info!("DHCP is now up!");

    info!("waiting for link up...");
    while !stack.is_link_up() {
        Timer::after_millis(500).await;
    }
    info!("Link is up!");

    info!("waiting for stack to be up...");
    stack.wait_config_up().await;
    info!("Stack is up!");

    let mut dht_pin: DHT22<'_, Delay> = DHT22::new(p.PIN_16, Delay);
    let delay_loop = Duration::from_secs(1800);

    // And now we can use it!
    loop {
        blink_n_times(&mut control, 1).await;
        let mut rx_buffer = [0; 8192];
        let mut tls_read_buffer = [0; 16640];
        let mut tls_write_buffer = [0; 16640];

        let client_state = TcpClientState::<1, 1024, 1024>::new();
        let tcp_client = TcpClient::new(stack, &client_state);
        let dns_client = DnsSocket::new(stack);
        let tls_config = TlsConfig::new(
            seed,
            &mut tls_read_buffer,
            &mut tls_write_buffer,
            TlsVerify::None,
        );

        let dht_reading = dht_pin.read().unwrap();

        let dht_reading_as_string: heapless::String<32> =
            match sensor_reading_to_string(dht_reading) {
                Ok(s) => s,
                Err(_e) => {
                    error!("Failed to convert sensor reading to string");
                    return; // handle the error
                }
            };
        info!("DHT reading as string: {:?}", &dht_reading_as_string);

        let mut http_client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);
        let bearer_token = get_api_key_as_bearer_string(environment_variables.supabase_key)
            .expect("Failed to get API key as bearer string");
        let headers = [
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("apikey", environment_variables.supabase_key),
            ("Authorization", bearer_token.as_str()),
        ];

        let mut request = match http_client
            .request(Method::POST, environment_variables.supabase_url)
            .await
        {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to make HTTP request: {:?}", e);
                return; // handle the error
            }
        }
        .headers(&headers)
        .body(dht_reading_as_string.as_bytes());
        let response = match request.send(&mut rx_buffer).await {
            Ok(resp) => resp,
            Err(_e) => {
                error!("Failed to send HTTP request");
                return; // handle the error;
            }
        };

        let body = match from_utf8(response.body().read_to_end().await.unwrap()) {
            Ok(b) => b,
            Err(_e) => {
                error!("Failed to read response body");
                return; // handle the error
            }
        };
        info!("Response body: {:?}", &body);
        Timer::after(delay_loop).await;
    }
}
