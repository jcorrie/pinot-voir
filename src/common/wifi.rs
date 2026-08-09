use crate::common::shared_functions::{EnvironmentVariables, blink_n_times};

use cyw43::Control;
use cyw43::JoinOptions;
use cyw43::{A4, Aligned, aligned_bytes};
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::Peri;
use embassy_rp::peripherals::{DMA_CH0, PIN_23, PIN_24, PIN_25, PIN_29, PIO0};
use embassy_rp::pio::Pio;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};
use reqwless::client::TlsConfig;
use static_cell::StaticCell;

pub const WEB_TASK_POOL_SIZE: usize = 12;

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
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
    async fn new(
        pin_23: Peri<'static, PIN_23>,
        pin_24: Peri<'static, PIN_24>,
        pin_25: Peri<'static, PIN_25>,
        pin_29: Peri<'static, PIN_29>,
        pio_0: Peri<'static, PIO0>,

        dma_ch0: Peri<'static, DMA_CH0>,
        spawner: Spawner,
    ) -> Self {
        let fw: &Aligned<A4, [u8]>;
        let clm: &[u8];
        // cyw43 0.7 no longer bundles the RP2040 NVRAM blob; it has to be passed to `new`.
        let nvram: &Aligned<A4, [u8]> = aligned_bytes!("../../cyw43-firmware/nvram_rp2040.bin");

        pub const FLASH_NEW_FIRMWARE: bool = true;

        match FLASH_NEW_FIRMWARE {
            true => {
                fw = aligned_bytes!("../../cyw43-firmware/43439A0.bin");
                clm = include_bytes!("../../cyw43-firmware/43439A0_clm.bin");
            }
            false => {
                // To make flashing faster for development, you may want to flash the firmwares independently
                // at hardcoded addresses, instead of baking them into the program with `include_bytes!`:
                //     probe-rs download 43439A0.bin --binary-format bin --chip RP2040 --base-address 0x10100000
                //     probe-rs download 43439A0_clm.bin --binary-format bin --chip RP2040 --base-address 0x10140000
                // SAFETY: 0x10100000 is 4-byte aligned, which is what `Aligned<A4, _>` requires.
                fw = unsafe {
                    &*(core::slice::from_raw_parts(0x10100000 as *const u8, 231077) as *const [u8]
                        as *const Aligned<A4, [u8]>)
                };
                clm = unsafe { core::slice::from_raw_parts(0x10140000 as *const u8, 984) };
            }
        }

        let pwr = Output::new(pin_23, Level::Low);
        let cs = Output::new(pin_25, Level::High);
        let config = Config::dhcpv4(Default::default());
        let mut pio = Pio::new(pio_0, crate::common::irqs::Irqs);
        let spi = PioSpi::new(
            &mut pio.common,
            pio.sm0,
            DEFAULT_CLOCK_DIVIDER,
            pio.irq0,
            cs,
            pin_24,
            pin_29,
            embassy_rp::dma::Channel::new(dma_ch0, crate::common::irqs::Irqs),
        );
        static STATE: StaticCell<cyw43::State> = StaticCell::new();
        let state = STATE.init(cyw43::State::new());
        let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
        spawner.spawn(defmt::unwrap!(wifi_task(runner)));

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

        spawner.spawn(defmt::unwrap!(net_task(runner)));

        Self {
            control,
            tls_config: None,
            stack,
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
    ) -> Self {
        let mut embassy_pico_wifi_core =
            EmbassyPicoWifiCore::new(pin_23, pin_24, pin_25, pin_29, pio0, dma_ch0, spawner).await;

        let successful_join = embassy_pico_wifi_core
            .join_wpa2_network(
                environment_variables.wifi_ssid,
                environment_variables.wifi_password,
            )
            .await;
        match successful_join {
            Ok(_) => {
                info!("Successfully joined network");
                blink_n_times(&mut embassy_pico_wifi_core.control, 1).await;
            }
            Err(_) => {
                info!("Failed to join network");
                blink_n_times(&mut embassy_pico_wifi_core.control, 3).await;
            }
        }

        embassy_pico_wifi_core
    }

    async fn join_wpa2_network(
        &mut self,
        wifi_ssid: &str,
        wifi_password: &str,
    ) -> Result<(), cyw43::JoinError> {
        info!("Joining network: {}", wifi_ssid);
        info!("Using password: {}", wifi_password);
        while let Err(err) = self
            .control
            .join(wifi_ssid, JoinOptions::new(wifi_password.as_bytes()))
            .await
        {
            info!("join failed: {}", err);
        }
        info!("waiting for link...");
        self.stack.wait_link_up().await;

        info!("waiting for DHCP...");
        self.stack.wait_config_up().await;

        info!("Stack is up!");

        info!(
            "Current IPv4 configuration: {}",
            self.stack.config_v4().unwrap().address
        );

        Ok(())
    }

    pub async fn disconnect_from_network(&mut self) {
        info!("Starting to disconnect from network.");
        self.control.leave().await;
        info!("Left network.");
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

#[embassy_executor::task]
pub async fn wifi_autoheal_task(
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    env: &'static EnvironmentVariables,
) {
    const RECONNECT_DELAY: Duration = Duration::from_secs(30);

    loop {
        info!("Checking WiFi connection status...");
        let mut wifi_core = shared_wifi_core.0.lock().await;

        // The most reliable way to test active connection is to poll google
        let ping_google_result = wifi_core
            .stack
            .dns_query("google.com", DnsQueryType::A)
            .await;

        if ping_google_result.is_err() {
            info!("WiFi link down, attempting reconnection...");
            match wifi_core
                .join_wpa2_network(env.wifi_ssid, env.wifi_password)
                .await
            {
                Ok(_) => info!("Rejoined WiFi."),
                Err(e) => info!("WiFi rejoin failed: {}", e),
            }
        } else {
            info!("WiFi is connected");
        }
        Timer::after(RECONNECT_DELAY).await;
    }
}
