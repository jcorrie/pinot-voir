//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

use cyw43::Control;
use defmt::*;
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration};
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::{DHT22, DHT22ReadingResponse};
use pinot_voir::common::shared_functions::{
    blink_n_times, parse_env_variables, EnvironmentVariables,
};
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, WEB_TASK_POOL_SIZE};

use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment},
};
use static_cell::make_static;

use {defmt_rtt as _, panic_probe as _};


type AppRouter = impl picoserve::routing::PathRouter<AppState>;


#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    id: usize,
    stack: &'static embassy_net::Stack<cyw43::NetDriver<'static>>,
    app: &'static picoserve::Router<AppRouter, AppState>,
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
    // And now we can use it!
    blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;

    blink_n_times(&mut embassy_pico_wifi_core.control, 2).await;
    fn make_app() -> picoserve::Router<AppRouter, AppState> {
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
                    let dht_reading = sensor.read().unwrap();
                    DHT22ReadingResponse {
                        temperature: dht_reading.get_temp(),
                        humidity: dht_reading.get_hum(),
                    }
                }),
            )
    }

    let app = make_static!(make_app());

    info!("Starting web server");

    let config = make_static!(picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Some(Duration::from_secs(5)),
        read_request: Some(Duration::from_secs(1)),
        write: Some(Duration::from_secs(1)),
    })
    .keep_connection_alive());

    let shared_control = SharedControl(make_static!(Mutex::new(embassy_pico_wifi_core.control)));
    let shared_sensor = SharedSensor(make_static!(Mutex::new(DHT22::new(p.PIN_16, Delay))));

    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 1) {
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
}

