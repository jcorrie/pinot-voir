use cyw43::Control;
use cyw43::JoinOptions;
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIN_23, PIN_24, PIN_25, PIN_29, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_time::Timer;
use embassy_rp::clocks::RoscRng;
use reqwless::client::TlsConfig;
use static_cell::StaticCell;

pub const WEB_TASK_POOL_SIZE: usize = 8;

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

pub struct EmbassyPicoWifiCore {
    pub control: Control<'static>,
    pub runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>,
    pub tls_config: Option<TlsConfig<'static>>,
    pub stack: Stack<'static>,
}

impl EmbassyPicoWifiCore {
    pub fn new(
        control: Control<'static>,
        runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>,
        stack: Stack<'static>,
    ) -> Self {
        Self {
            control,
            runner: runner,
            tls_config: None,
            stack: stack,
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

        // Init network stack
 
        static RESOURCES: StaticCell<StackResources<WEB_TASK_POOL_SIZE>> = StaticCell::new();
        let mut rng = RoscRng;
        let seed = rng.next_u64();

        let (stack, runner) = embassy_net::new(
            net_device,
            config,
            RESOURCES.init(StackResources::new()),
            seed,
        );

        // spawner
        //     .spawn(net_task(&runner))
        //     .expect("failed to spawn net_task");

        EmbassyPicoWifiCore::new(control, runner, stack)
    }

    pub async fn join_wpa2_network(
        &mut self,
        wifi_ssid: &str,
        wifi_password: &str,
    ) -> Result<(), cyw43::ControlError> {
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

        // And now we can use it!
        info!("Stack is up!");

        // And now we can use it!
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
