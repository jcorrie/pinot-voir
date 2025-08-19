use crate::common::shared_functions::{EnvironmentVariables, blink_n_times, parse_env_variables};

use cyw43::Control;
use cyw43::JoinOptions;
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIN_23, PIN_24, PIN_25, PIN_29, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Duration, Timer};
use reqwless::client::TlsConfig;
use static_cell::StaticCell;

pub const WEB_TASK_POOL_SIZE: usize = 12;

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
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

#[derive(Clone, Copy)]
pub struct SharedEmbassyWifiPicoCore(
    pub &'static Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>,
);

pub struct EmbassyPicoWifiCore {
    pub control: Control<'static>,
    pub tls_config: Option<TlsConfig<'static>>,
    pub stack: Stack<'static>,
}

impl EmbassyPicoWifiCore {
    pub fn new(control: Control<'static>, stack: Stack<'static>) -> Self {
        Self {
            control,
            tls_config: None,
            stack,
        }
    }

    pub async fn initiate_wifi_prelude(
        pin_23: Peri<'static, PIN_23>,
        pin_24: Peri<'static, PIN_24>,
        pin_25: Peri<'static, PIN_25>,
        pin_29: Peri<'static, PIN_29>,
        pio_0: Peri<'static, PIO0>,
        dma_ch0: Peri<'static, DMA_CH0>,
        spawner: Spawner,
    ) -> Self {
        let fw = include_bytes!("../../cyw43-firmware/43439A0.bin");
        let clm = include_bytes!("../../cyw43-firmware/43439A0_clm.bin");
        let pwr = Output::new(pin_23, Level::Low);
        let cs = Output::new(pin_25, Level::High);
        let config = Config::dhcpv4(Default::default());
        let mut pio = Pio::new(pio_0, Irqs);
        let spi = PioSpi::new(
            &mut pio.common,
            pio.sm0,
            DEFAULT_CLOCK_DIVIDER,
            pio.irq0,
            cs,
            pin_24,
            pin_29,
            dma_ch0,
        );
        static STATE: StaticCell<cyw43::State> = StaticCell::new();
        let state = STATE.init(cyw43::State::new());
        let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
        spawner
            .spawn(wifi_task(runner))
            .expect("failed to spawn wifi_task");

        control.init(clm).await;
        control
            .set_power_management(cyw43::PowerManagementMode::PowerSave)
            .await;

        static RESOURCES: StaticCell<StackResources<WEB_TASK_POOL_SIZE>> = StaticCell::new();
        let mut rng = RoscRng;
        let seed = rng.next_u64();

        let (stack, runner) = embassy_net::new(
            net_device,
            config,
            RESOURCES.init(StackResources::new()),
            seed,
        );

        spawner
            .spawn(net_task(runner))
            .expect("failed to spawn net_task");

        EmbassyPicoWifiCore::new(control, stack)
    }

    pub async fn join_wpa2_network(
        &mut self,
        wifi_ssid: &str,
        wifi_password: &str,
    ) -> Result<(), cyw43::ControlError> {
        info!("Joining network: {}", wifi_ssid);
        info!("Using password: {}", wifi_password);
        while let Err(err) = self
            .control
            .join(wifi_ssid, JoinOptions::new(wifi_password.as_bytes()))
            .await
        {
            info!("join failed with status={}", err.status);
        }
        info!("waiting for link...");
        self.stack.wait_link_up().await;

        info!("waiting for DHCP...");
        self.stack.wait_config_up().await;

        info!("Stack is up!");
        Ok(())
    }
}

pub struct HttpBuffers {
    pub rx_buffer: [u8; 8192],
    pub tls_read_buffer: [u8; 8192],
    pub tls_write_buffer: [u8; 8192],
}

impl Default for HttpBuffers {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpBuffers {
    pub fn new() -> Self {
        Self {
            rx_buffer: [0; 8192],
            tls_read_buffer: [0; 8192],
            tls_write_buffer: [0; 8192],
        }
    }
}

pub async fn connect_to_network(
    pin_23: Peri<'static, PIN_23>,
    pin_24: Peri<'static, PIN_24>,
    pin_25: Peri<'static, PIN_25>,
    pin_29: Peri<'static, PIN_29>,
    pio0: Peri<'static, PIO0>,
    dma_ch0: Peri<'static, DMA_CH0>,
    spawner: Spawner,
    environment_variables: &EnvironmentVariables,
) -> EmbassyPicoWifiCore {
    let mut embassy_pico_wifi_core = EmbassyPicoWifiCore::initiate_wifi_prelude(
        pin_23, pin_24, pin_25, pin_29, pio0, dma_ch0, spawner,
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

    embassy_pico_wifi_core
}

#[embassy_executor::task]
pub async fn rejoin_wifi_loop_task(
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    env: &'static EnvironmentVariables,
) {
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);
    loop {
        let mut wifi_core = shared_wifi_core.0.lock().await;
        if !wifi_core.stack.is_link_up() {
            info!("WiFi link down, attempting reconnection...");
            match wifi_core
                .join_wpa2_network(env.wifi_ssid, env.wifi_password)
                .await
            {
                Ok(_) => info!("Rejoined WiFi."),
                Err(e) => info!("WiFi rejoin failed: status={}", e.status),
            }
        }
        // Use stack here
        Timer::after(RECONNECT_DELAY).await;
    }
}
