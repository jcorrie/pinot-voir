//! Periodically read a DHT22 sensor and send the data to a Supabase database using an HTTP POST request.

#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![allow(async_fn_in_trait)]

use defmt::{error, info};
use embassy_dht::dht22::DHT22;
use embassy_executor::Spawner;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::TcpConnection;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_rp::clocks::RoscRng;
use embassy_time::{Delay, Duration, Timer};
use pinot_voir::common::shared_functions::{EnvironmentVariables, blink_n_times};
use pinot_voir::common::supabase::{construct_post_request_arguments, read_http_response};
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, HttpBuffers};
use reqwless::client::{HttpClient, HttpConnection, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};
use reqwless::response::Response;
use static_cell::make_static;

use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        make_static!(EnvironmentVariables::new());
    let p = embassy_rp::init(Default::default());
    // Wifi prelude
    info!("Hello World!");

    let mut embassy_pico_wifi_core = EmbassyPicoWifiCore::connect_to_network(
        p.PIN_23,
        p.PIN_24,
        p.PIN_25,
        p.PIN_29,
        p.PIO0,
        p.DMA_CH0,
        spawner,
        environment_variables,
    )
    .await;

    blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;
    let mut rng = RoscRng;
    let seed = rng.next_u64();

    let mut http_buffers = HttpBuffers::new();
    let tls_config = TlsConfig::new(
        seed,
        &mut http_buffers.tls_read_buffer,
        &mut http_buffers.tls_write_buffer,
        TlsVerify::None,
    );

    let client_state: TcpClientState<1, 1024, 1024> = TcpClientState::<1, 1024, 1024>::new();
    let tcp_client = TcpClient::new(embassy_pico_wifi_core.stack, &client_state);
    let dns_client = DnsSocket::new(embassy_pico_wifi_core.stack);
    let mut http_client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);

    let mut dht_pin: DHT22<'_, Delay> = DHT22::new(p.PIN_16, Delay);
    let delay_loop = Duration::from_secs(1800);

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
        let response: Response<'_, '_, HttpConnection<'_, TcpConnection<'_, 1, 1024, 1024>>> =
            match request.send(&mut http_buffers.rx_buffer).await {
                Ok(resp) => resp,
                Err(_e) => {
                    error!("Failed to send HTTP request");
                    return; // handle the error;
                }
            };

        read_http_response(response).await;
        Timer::after(delay_loop).await;
    }
}
