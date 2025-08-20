//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.
//! Additionally, send the readings to a Supabase database on a loop.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
use defmt::*;
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration};
use picoserve::extract::Json;
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::DHT22;
use pinot_voir::common::sensor_tools::SensorState;
use pinot_voir::common::shared_functions::{EnvironmentVariables, blink_n_times};
use pinot_voir::common::wifi::{
    EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore, WEB_TASK_POOL_SIZE, wifi_autoheal_task,
};

use picoserve::{
    AppRouter, AppWithStateBuilder,
    response::DebugValue,
    routing::{PathRouter, get, parse_path_segment},
};

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
                "/disconnect",
                get(|State(app_state): State<AppState>| async move {
                    info!("Received disconnect");
                    app_state
                        .shared_wifi_core
                        .0
                        .lock()
                        .await
                        .disconnect_from_network()
                        .await;
                }),
            )
            .route(
                ("/set_led", parse_path_segment()),
                get(
                    |led_is_on,
                     State(SharedEmbassyWifiPicoCore(wifi_core)): State<
                        SharedEmbassyWifiPicoCore,
                    >| async move {
                        wifi_core.lock().await.control.gpio_set(0, led_is_on).await;

                        DebugValue(led_is_on)
                    },
                ),
            )
            .route(
                "/read_sensor",
                get(|State(app_state): State<AppState>| async move {
                    let mut sensor = app_state.shared_sensor.0.lock().await;
                    let dht_reading = sensor.read();
                    match dht_reading {
                        Ok(dht_reading) => {
                            let mut sensor_state_lock =
                                app_state.shared_sensor_state.0.lock().await;
                            sensor_state_lock.humidity = Some(dht_reading.get_hum());
                            sensor_state_lock.temperature = Some(dht_reading.get_temp());
                        }
                        Err(_e) => info!(
                            "Error reading sensor - likely because of two reads close together."
                        ),
                    }

                    let sensor_state = app_state.shared_sensor_state.0.lock().await;
                    Json(*sensor_state)
                }),
            )
        // ...existing code...
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
struct SharedSensor<D: 'static>(&'static Mutex<CriticalSectionRawMutex, DHT22<'static, D>>);

#[derive(Clone, Copy)]
struct SharedSensorsState(&'static Mutex<CriticalSectionRawMutex, SensorState>);

struct AppState {
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    shared_sensor: SharedSensor<Delay>,
    shared_sensor_state: SharedSensorsState,
}

impl picoserve::extract::FromRef<AppState> for SharedEmbassyWifiPicoCore {
    fn from_ref(state: &AppState) -> Self {
        state.shared_wifi_core
    }
}

impl picoserve::extract::FromRef<AppState> for SharedSensor<Delay> {
    fn from_ref(state: &AppState) -> Self {
        state.shared_sensor.clone()
    }
}

impl picoserve::extract::FromRef<AppState> for SharedSensorsState {
    fn from_ref(state: &AppState) -> Self {
        state.shared_sensor_state
    }
}

impl picoserve::extract::FromRef<AppState> for AppState {
    fn from_ref(state: &AppState) -> Self {
        AppState {
            shared_wifi_core: state.shared_wifi_core,
            shared_sensor: state.shared_sensor.clone(),
            shared_sensor_state: state.shared_sensor_state,
        }
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

    let shared_wifi_core: SharedEmbassyWifiPicoCore =
        SharedEmbassyWifiPicoCore(make_static!(Mutex::new(embassy_pico_wifi_core)));
    let shared_sensor = SharedSensor(make_static!(Mutex::new(DHT22::new(p.PIN_16, Delay))));
    let shared_sensor_state = SharedSensorsState(make_static!(Mutex::new(SensorState::new())));

    spawner
        .spawn(wifi_autoheal_task(shared_wifi_core, environment_variables))
        .unwrap();

    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 3) {
        spawner.must_spawn(web_task(
            id,
            shared_wifi_core.0.lock().await.stack,
            app,
            config,
            AppState {
                shared_wifi_core,
                shared_sensor: shared_sensor.clone(),
                shared_sensor_state,
            },
        ));
    }

    info!("Web server started");
}
