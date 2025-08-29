//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.
//! Additionally, send the readings to a Supabase database on a loop.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
use bytemuck;
use core::fmt::{Error, Write};
use core::str::from_utf8;
use cyw43::Control;
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::Peri;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, DMA_CH1, PIN_26};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Timer;
use embassy_time::{Delay, Duration};
use heapless::String;
use picoserve::extract::Json;
use picoserve::extract::State;
use picoserve::{
    AppRouter, AppWithStateBuilder,
    response::DebugValue,
    routing::{PathRouter, get, parse_path_segment},
};
use pinot_voir::common::dht22_tools::DHT22;
use pinot_voir::common::sensor_tools::SensorState;
use pinot_voir::common::shared_functions::{EnvironmentVariables, blink_n_times};
use pinot_voir::common::wifi::{
    EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore, WEB_TASK_POOL_SIZE, wifi_autoheal_task,
};

use static_cell::make_static;

use {defmt_rtt as _, panic_probe as _};

struct AppProps;

impl AppWithStateBuilder for AppProps {
    type State = AppState;
    type PathRouter = impl PathRouter<AppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        picoserve::Router::new()
            .route("/", get(|| async move { "Hello world 2." }))
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

bind_interrupts!(
    /// Binds the ADC interrupts.
    struct Irqs {
        ADC_IRQ_FIFO => InterruptHandler;
    }
);

#[embassy_executor::task()]
async fn udp_stream(
    app: &'static AppRouter<AppProps>,
    state: AppState,
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    pin_26: Peri<'static, PIN_26>,
    adc: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH1>,
) -> ! {
    let port = 1234;
    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];
    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];

    let mut adc = Adc::new(adc, Irqs, Config::default());
    let mut dma = dma; // We meed this to be mutable
    let mut p26 = Channel::new_pin(pin_26, Pull::None);
    // loop {
    //     let level = adc.read(&mut p26).await.unwrap();
    //     let mut msg: String<32> = String::new();
    //     write!(msg, "{:.2}", level).unwrap();
    //     let msg_bytes = msg.as_bytes();
    //     socket
    //         .send_to(msg_bytes, broadcast_addr)
    //         .await
    //         .expect("Could not send UDP data");
    //     Timer::after_millis(1).await;
    // }

    let sample_frequency_s: u64 = 44100000;
    let broadcast_addr = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), port);
    let mut socket = UdpSocket::new(
        shared_wifi_core.0.lock().await.stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(port).expect("Could not bind UDP sensor.");

    const NUM_CHANNELS: usize = 1;
    const MAX_UDP_PAYLOAD: usize = 1024;
    const BUFFER_SIZE: usize = 1024;
    const SAMPLE_RATE_HZ: u32 = 44100;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;
    // let msg = b"Hello, world!";
    loop {
        // socket.send_to(msg, broadcast_addr).await.unwrap();
        let mut audio_buffer: [u16; BUFFER_SIZE] = [0_u16; BUFFER_SIZE];
        match adc
            .read_many(&mut p26, &mut audio_buffer, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                let audio_bytes = bytemuck::cast_slice(&audio_buffer);

                // Send data in chunks if it's too large for a single UDP packet
                for chunk in audio_bytes.chunks(MAX_UDP_PAYLOAD) {
                    match socket.send_to(chunk, broadcast_addr).await {
                        Ok(_) => {}
                        Err(e) => {
                            info!("UDP send error: {:?}", e);
                            break; // Break inner loop on error
                        }
                    }

                    // Small delay between chunks to avoid overwhelming the network
                    Timer::after_micros(100).await;
                }
            }
            Err(e) => {
                info!("ADC read error: {:?}", e);
                Timer::after_millis(10).await;
            }
        }
    }
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

    spawner
        .spawn(udp_stream(
            app,
            AppState {
                shared_wifi_core,
                shared_sensor: shared_sensor.clone(),
                shared_sensor_state,
            },
            shared_wifi_core,
            p.PIN_26,
            p.ADC,
            p.DMA_CH1,
        ))
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
