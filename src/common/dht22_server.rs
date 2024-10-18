use cyw43::Control;
use embassy_dht::dht22::DHT22;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration, Timer};
use picoserve::extract::State;

use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment},
};
use reqwless::client::{HttpClient, HttpConnection, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};

use super::dht22_tools::DHT22ReadingResponse;

#[derive(Clone, Copy)]
pub struct SharedControl(pub &'static Mutex<CriticalSectionRawMutex, Control<'static>>);

#[derive(Clone, Copy)]
pub struct SharedSensor<D: 'static>(pub &'static Mutex<CriticalSectionRawMutex, DHT22<'static, D>>);

pub struct AppState {
    pub shared_control: SharedControl,
    pub shared_sensor: SharedSensor<Delay>,
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
pub type AppRouter = impl picoserve::routing::PathRouter<AppState>;
pub fn make_app() -> picoserve::Router<AppRouter, AppState> {
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
                    Err(_) => Err(
                        "Error reading sensor - likely because two reads were too close together",
                    ),
                }
            }),
        )
}
