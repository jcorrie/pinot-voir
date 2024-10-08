//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.
//! Additionally, send the readings to a Supabase database on a loop.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

use cyw43::Control;
use cyw43_pio::PioSpi;
use defmt::*;
use embassy_dht::Reading;
use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIN_25, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration, Timer};
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::DHT22;
use pinot_voir::common::shared_functions::{
    blink_n_times, parse_env_variables, EnvironmentVariables,
};
use pinot_voir::common::wifi::{initiate_wifi_prelude, EmbassyPicoWifiCore, WEB_TASK_POOL_SIZE};

use picoserve::response::IntoResponse;
use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment},
};
use rand::Rng;
use static_cell::make_static;
use static_cell::StaticCell;

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

    let mut wifi_core: EmbassyPicoWifiCore = initiate_wifi_prelude(
        p.PIN_23,
        p.PIN_24,
        p.PIN_25,
        p.PIN_29,
        p.PIO0,
        p.DMA_CH0,
        spawner,
    )
    .await;

    loop {
        match wifi_core
            .control
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
    blink_n_times(&mut wifi_core.control, 1).await;
    // Wait for DHCP, not necessary when using static IP
    info!("waiting for DHCP...");
    while !wifi_core.stack.is_config_up() {
        Timer::after_millis(100).await;
    }
    info!("DHCP is now up!");

    info!("waiting for link up...");
    while !wifi_core.stack.is_link_up() {
        Timer::after_millis(500).await;
    }
    info!("Link is up!");

    info!("waiting for stack to be up...");
    wifi_core.stack.wait_config_up().await;
    info!("Stack is up!");

    // And now we can use it!
    blink_n_times(&mut wifi_core.control, 1).await;

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
                    let dht_reading = sensor.read();
                    match dht_reading {
                        Ok(dht_reading) => Ok(DHT22Reading {
                            temperature: dht_reading.get_temp(),
                            humidity: dht_reading.get_hum(),
                        }),
                        Err(_) => Err("Error reading sensor - likely because two reads were too close together"),
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

    let shared_control = SharedControl(make_static!(Mutex::new(wifi_core.control)));
    let shared_sensor = SharedSensor(make_static!(Mutex::new(DHT22::new(p.PIN_16, Delay))));

    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 1) {
        spawner.must_spawn(web_task(
            id,
            wifi_core.stack,
            app,
            config,
            AppState {
                shared_control,
                shared_sensor: shared_sensor.clone(),
            },
        ));
    }

    info!("Web server started");

    unwrap!(spawner.spawn(toggle_led(Duration::from_secs(1), shared_sensor)));
}

struct DHT22Reading<T: core::fmt::Display> {
    pub temperature: T,
    pub humidity: T,
}

impl<T: core::fmt::Display> IntoResponse for DHT22Reading<T> {
    async fn write_to<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        format_args!(
            "{{\"temperature\":{},\"humidity\":{}}}",
            self.temperature, self.humidity
        )
        .write_to(connection, response_writer)
        .await
    }
}

#[embassy_executor::task(pool_size = 2)]
async fn toggle_led(delay: Duration, sensor: SharedSensor<Delay>) {
    let delay_loop = Duration::from_secs(7);
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
        Timer::after(delay_loop).await;
    }
}
