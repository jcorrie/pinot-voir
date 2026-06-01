//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.
//! Additionally, send the readings to a Supabase database on a loop.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![allow(non_exhaustive_patterns)] // LSP false positive from #[embassy_executor::main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH2};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration};
use picoserve::extract::Json;
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::DHT22;
use pinot_voir::common::sensor_tools::SensorState;
use pinot_voir::common::shared_functions::{blink_n_times, EnvironmentVariables};
use pinot_voir::common::wifi::{
    wifi_autoheal_task, EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore, WEB_TASK_POOL_SIZE,
};

use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment, PathRouter},
    AppRouter, AppWithStateBuilder,
};

use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

struct AppProps {
    shared_wifi_core: SharedEmbassyWifiPicoCore,
}

impl AppWithStateBuilder for AppProps {
    type State = AppState;
    type PathRouter = impl PathRouter<AppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let wifi_core = self.shared_wifi_core;
        picoserve::Router::new()
            .route("/", get(|| async move { "Hello world 2." }))
            .route(
                ("/set_led", parse_path_segment::<bool>()),
                get(move |led_is_on: bool| async move {
                    info!("set_led handler called: {}", led_is_on);
                    wifi_core.0.lock().await.control.gpio_set(0, led_is_on).await;
                    info!("set_led gpio_set done");
                    DebugValue(led_is_on)
                }),
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
    config: &'static picoserve::Config,
    state: AppState,
) -> ! {
    let port = 80;
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];

    let app_shared = app.shared();
    let app_with_state = app_shared.with_state(&state);
    picoserve::Server::new(&app_with_state, config, &mut http_buffer)
        .listen_and_serve(id, stack, port, &mut tcp_rx_buffer, &mut tcp_tx_buffer)
        .await
        .into_never()
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

bind_interrupts!(struct WifiIrqs {
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH2>;
});

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();
static APP: StaticCell<AppRouter<AppProps>> = StaticCell::new();
static CONFIG: StaticCell<picoserve::Config> = StaticCell::new();
static WIFI_CORE: StaticCell<Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>> =
    StaticCell::new();
static SENSOR: StaticCell<Mutex<CriticalSectionRawMutex, DHT22<'static, Delay>>> =
    StaticCell::new();
static SENSOR_STATE: StaticCell<Mutex<CriticalSectionRawMutex, SensorState>> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        ENV.init(EnvironmentVariables::new());
    let p = embassy_rp::init(Default::default());
    // Wifi prelude
    info!("Hello World!");

    let mut embassy_pico_wifi_core = EmbassyPicoWifiCore::connect_to_network(
        p.PIN_23,
        p.PIN_24,
        p.PIN_25,
        p.PIN_29,
        p.PIO0,
        dma::Channel::new(p.DMA_CH0, WifiIrqs),
        dma::Channel::new(p.DMA_CH2, WifiIrqs),
        spawner,
        environment_variables,
    )
    .await;

    // And now we can use it!
    blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;

    let shared_wifi_core: SharedEmbassyWifiPicoCore =
        SharedEmbassyWifiPicoCore(WIFI_CORE.init(Mutex::new(embassy_pico_wifi_core)));

    let app: &'static AppRouter<AppProps> = APP.init(AppProps { shared_wifi_core }.build_app());

    info!("Starting web server");

    let config: &'static picoserve::Config = CONFIG.init(
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Duration::from_secs(5),
            persistent_start_read_request: Duration::from_secs(1),
            read_request: Duration::from_secs(1),
            write: Duration::from_secs(1),
        })
        .keep_connection_alive(),
    );
    let shared_sensor = SharedSensor(SENSOR.init(Mutex::new(DHT22::new(p.PIN_16, Delay))));
    let shared_sensor_state = SharedSensorsState(SENSOR_STATE.init(Mutex::new(SensorState::new())));
    
    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 3) {
        spawner.spawn(web_task(
            id,
            shared_wifi_core.0.lock().await.stack,
            app,
            config,
            AppState {
                shared_wifi_core,
                shared_sensor: shared_sensor.clone(),
                shared_sensor_state,
            },
        ).unwrap());
    }

    spawner.spawn(wifi_autoheal_task(shared_wifi_core, environment_variables).unwrap());

    info!("Web server started");
}
