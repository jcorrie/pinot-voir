//! Create a server using picoserver on a Raspberry Pi Pico W.
//! Read the DHT22 sensor and expose the temperature and humidity readings via the server.


#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

use cyw43::Control;
use cyw43_pio::PioSpi;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration, Timer};
use picoserve::extract::State;
use pinot_voir::common::dht22_tools::DHT22;
use pinot_voir::common::shared_functions::{blink_n_times, parse_env_variables, EnvironmentVariables};

use picoserve::response::IntoResponse;
use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment},
};
use rand::Rng;
use static_cell::make_static;
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

type AppRouter = impl picoserve::routing::PathRouter<AppState>;

const WEB_TASK_POOL_SIZE: usize = 8;

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
    unwrap!(spawner.spawn(wifi_task(runner)));

    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    let config = Config::dhcpv4(Default::default());

    // Init network stack
    static STACK: StaticCell<Stack<cyw43::NetDriver<'static>>> = StaticCell::new();
    static RESOURCES: StaticCell<StackResources<WEB_TASK_POOL_SIZE>> = StaticCell::new();
    let stack = &*STACK.init(Stack::new(
        net_device,
        config,
        RESOURCES.init(StackResources::<WEB_TASK_POOL_SIZE>::new()),
        embassy_rp::clocks::RoscRng.gen(),
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
    blink_n_times(&mut control, 1).await;
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

    // And now we can use it!
    blink_n_times(&mut control, 1).await;


    blink_n_times(&mut control, 2).await;
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
                    DHT22Reading {
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

    let shared_control = SharedControl(make_static!(Mutex::new(control)));
    let shared_sensor = SharedSensor(make_static!(Mutex::new(DHT22::new(p.PIN_16, Delay))));

    // for some reason, idk why, I can only spawn one less than the pool size
    // otherwise it panics
    for id in 1..(WEB_TASK_POOL_SIZE - 1) {
        spawner.must_spawn(web_task(
            id,
            stack,
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
