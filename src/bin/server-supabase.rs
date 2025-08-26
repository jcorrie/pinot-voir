//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.
//! Additionally, send the readings to a Supabase database on a loop.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use cyw43::Control;
use defmt::*;
use embassy_dht::Reading;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::TcpConnection;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_rp::clocks::RoscRng;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration, Timer};
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::{DHT22, DHT22ReadingResponse};
use pinot_voir::common::shared_functions::{EnvironmentVariables, blink_n_times};
use pinot_voir::common::supabase::{construct_post_request_arguments, read_http_response};
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, HttpBuffers, WEB_TASK_POOL_SIZE};

use picoserve::{
    AppRouter, AppWithStateBuilder,
    response::DebugValue,
    routing::{PathRouter, get, parse_path_segment},
};
use reqwless::client::{HttpClient, HttpConnection, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};
use reqwless::response::Response;
use static_cell::make_static;

use {defmt_rtt as _, panic_probe as _};

struct AppProps;

impl AppWithStateBuilder for AppProps {
    type State = AppState;
    type PathRouter = impl PathRouter<AppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        picoserve::Router::new()
            .route("/", get(|| async move { "Hello world." }))
            .route(
                ("/set_led", parse_path_segment()),
                get(
                    |led_is_on, State(SharedControl(control)): State<SharedControl>| async move {
                        control.lock().await.gpio_set(0, led_is_on).await;

                        DebugValue(led_is_on)
                    },
                ),
            )
            .route(
                "/read_sensor",
                get(|State(SharedSensor(shared_sensor))| async move {
                    let mut sensor = shared_sensor.lock().await;
                    let dht_reading = sensor.read();
                    match dht_reading {
                        Ok(dht_reading) => Ok(DHT22ReadingResponse {
                            temperature: dht_reading.get_temp(),
                            humidity: dht_reading.get_hum(),
                        }),
                        Err(_) => Err("Error reading sensor - likely because two reads were too close together"),
                    }
                }),
            )
    }
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
    config: &'static picoserve::Config<Duration>,
    state: AppState,
) -> ! {
    let port = 80;
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];

    picoserve::listen_and_serve_with_state(
        id,
        app,
        config,
        stack,
        port,
        &mut tcp_rx_buffer,
        &mut tcp_tx_buffer,
        &mut http_buffer,
        &state,
    )
    .await
}

#[derive(Clone, Copy)]
struct SharedControl(&'static Mutex<CriticalSectionRawMutex, Control<'static>>);

#[derive(Clone, Copy)]
struct SharedSensor<D: 'static>(&'static Mutex<CriticalSectionRawMutex, DHT22<'static, D>>);

struct AppState {
    shared_control: SharedControl,
    shared_sensor: SharedSensor<Delay>,
}

impl picoserve::extract::FromRef<AppState> for SharedControl {
    fn from_ref(state: &AppState) -> Self {
        state.shared_control
    }
}

impl picoserve::extract::FromRef<AppState> for SharedSensor<Delay> {
    fn from_ref(state: &AppState) -> Self {
        state.shared_sensor.clone()
    }
}

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

    // And now we can use it!
    blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;

    let app = make_static!(AppProps.build_app());

    info!("Starting web server");

    let config = make_static!(
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Some(Duration::from_secs(5)),
            persistent_start_read_request: Some(Duration::from_secs(1)),
            read_request: Some(Duration::from_secs(1)),
            write: Some(Duration::from_secs(1)),
        })
        .keep_connection_alive()
    );

    let shared_control: SharedControl =
        SharedControl(make_static!(Mutex::new(embassy_pico_wifi_core.control)));
    let shared_sensor = SharedSensor(make_static!(Mutex::new(DHT22::new(p.PIN_16, Delay))));

    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 3) {
        spawner.must_spawn(web_task(
            id,
            embassy_pico_wifi_core.stack,
            app,
            config,
            AppState {
                shared_control,
                shared_sensor: shared_sensor.clone(),
            },
        ));
    }

    info!("Web server started");

    unwrap!(spawner.spawn(read_sensor(
        shared_sensor,
        environment_variables,
        embassy_pico_wifi_core.stack,
    )));
}

#[embassy_executor::task(pool_size = 1)]
async fn read_sensor(
    sensor: SharedSensor<Delay>,
    environment_variables: &'static EnvironmentVariables,
    stack: Stack<'static>,
) {
    let mut rng = RoscRng;
    let seed = rng.next_u64();
    let mut http_buffers: HttpBuffers = HttpBuffers::new();
    let client_state: TcpClientState<1, 1024, 1024> = TcpClientState::<1, 1024, 1024>::new();

    let tls_config: TlsConfig<'_> = TlsConfig::new(
        seed,
        &mut http_buffers.tls_read_buffer,
        &mut http_buffers.tls_write_buffer,
        TlsVerify::None,
    );
    let tcp_client = TcpClient::new(stack, &client_state);
    let dns_client = DnsSocket::new(stack);
    let mut http_client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);

    let delay_loop = Duration::from_secs(60 * 30);
    let blank_reading = Reading {
        temp: 0.0,
        hum: 0.0,
    };
    loop {
        let dht_reading = sensor.0.lock().await.read().unwrap_or(blank_reading);
        info!(
            "Temp = {}, Humi = {}",
            dht_reading.get_temp(),
            dht_reading.get_hum()
        );
        let (dht_reading_as_string, headers) =
            construct_post_request_arguments(dht_reading, environment_variables)
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
