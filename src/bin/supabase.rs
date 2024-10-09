//! Periodically read a DHT22 sensor and send the data to a Supabase database using an HTTP POST request.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]

use core::fmt::{Error, Write};
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
use heapless::String;
use pinot_voir::common::dht22_tools::sensor_reading_to_string;
use pinot_voir::common::shared_functions::{
    blink_n_times, parse_env_variables, EnvironmentVariables,
};
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, HttpBuffers};
use rand::RngCore;
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: EnvironmentVariables = parse_env_variables();
    let p = embassy_rp::init(Default::default());
    // Wifi prelude
    info!("Hello World!");

    let mut embassy_pico_wifi_core = EmbassyPicoWifiCore::initiate_wifi_prelude(
        p.PIN_23, p.PIN_24, p.PIN_25, p.PIN_29, p.PIO0, p.DMA_CH0, spawner,
    )
    .await;

    let successful_join = embassy_pico_wifi_core
        .join_wpa2_network(
            environment_variables.wifi_ssid,
            environment_variables.wifi_password,
        )
        .await;
    match successful_join {
        Ok(_) => info!("Successfully joined network"),
        Err(_) => info!("Failed to join network"),
    }
    let mut rng = RoscRng;
    // Generate random seed
    let seed = rng.next_u64();

    let mut dht_pin: DHT22<'_, Delay> = DHT22::new(p.PIN_16, Delay);
    let delay_loop = Duration::from_secs(1800);

    // And now we can use it!
    blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;
    let mut http_buffers = HttpBuffers {
        rx_buffer: [0; 8192],
        tls_read_buffer: [0; 16640],
        tls_write_buffer: [0; 16640],
    };
    let tls_config = TlsConfig::new(
        seed,
        &mut http_buffers.tls_read_buffer,
        &mut http_buffers.tls_write_buffer,
        TlsVerify::None,
    );

    let client_state = TcpClientState::<1, 1024, 1024>::new();
    let tcp_client = TcpClient::new(embassy_pico_wifi_core.stack, &client_state);
    let dns_client = DnsSocket::new(embassy_pico_wifi_core.stack);

    let mut http_client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);
    loop {
        let dht_reading = dht_pin.read().unwrap();
        let (dht_reading_as_string, headers) =
            construct_post_request_arguments(dht_reading, &environment_variables)
                .expect("Failed to read dht reading");
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
        let response = match request.send(&mut http_buffers.rx_buffer).await {
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

fn construct_post_request_arguments(
    dht_reading: embassy_dht::Reading<f32, f32>,
    environment_variables: &EnvironmentVariables,
) -> Result<(heapless::String<32>, [(&str, &str); 3]), core::fmt::Error> {
    let dht_reading_as_string: heapless::String<32> = match sensor_reading_to_string(dht_reading) {
        Ok(s) => s,
        Err(_e) => {
            error!("Failed to convert sensor reading to string");
            return Err(_e);
        }
    };
    info!("DHT reading as string: {:?}", &dht_reading_as_string);

    let headers: [(&str, &str); 3] = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("apikey", environment_variables.supabase_key),
        ("Authorization", environment_variables.supabase_key),
    ];
    Ok((dht_reading_as_string, headers))
}
