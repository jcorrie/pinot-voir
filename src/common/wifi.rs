use cyw43::Control;
use cyw43_pio::PioSpi;
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIN_23, PIN_24, PIN_25, PIN_29, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_time::Timer;
use embedded_nal_async::{Dns, TcpConnect};

use rand::{Rng, RngCore};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
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
async fn net_task(stack: &'static Stack<cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

pub struct EmbassyPicoWifiCore {
    pub control: Control<'static>,
    pub stack: &'static Stack<cyw43::NetDriver<'static>>,
    pub tls_config: Option<TlsConfig<'static>>,
}

impl EmbassyPicoWifiCore {
    pub fn new(
        control: Control<'static>,
        stack: &'static Stack<cyw43::NetDriver<'static>>,
    ) -> Self {
        Self {
            control,
            stack,
            tls_config: None,
        }
    }

    pub async fn initiate_wifi_prelude(
        pin_23: PIN_23,
        pin_24: PIN_24,
        pin_25: PIN_25,
        pin_29: PIN_29,
        pio_0: PIO0,
        dma_ch0: DMA_CH0,
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
        static STACK: StaticCell<Stack<cyw43::NetDriver<'static>>> = StaticCell::new();
        static RESOURCES: StaticCell<StackResources<WEB_TASK_POOL_SIZE>> = StaticCell::new();
        let stack = &*STACK.init(Stack::new(
            net_device,
            config,
            RESOURCES.init(StackResources::<WEB_TASK_POOL_SIZE>::new()),
            embassy_rp::clocks::RoscRng.gen(),
        ));

        spawner
            .spawn(net_task(stack))
            .expect("failed to spawn net_task");

        EmbassyPicoWifiCore::new(control, stack)
    }

    pub async fn join_wpa2_network(
        &mut self,
        wifi_ssid: &str,
        wifi_password: &str,
    ) -> Result<(), cyw43::ControlError> {
        loop {
            match self.control.join_wpa2(wifi_ssid, wifi_password).await {
                Ok(_) => break,
                Err(err) => {
                    info!("join failed with status={}", err.status);
                    return Err(err);
                }
            }
        }
        // Wait for DHCP, not necessary when using static IP
        info!("waiting for DHCP...");
        while !self.stack.is_config_up() {
            Timer::after_millis(100).await;
        }
        info!("DHCP is now up!");

        info!("waiting for link up...");
        while !self.stack.is_link_up() {
            Timer::after_millis(500).await;
        }
        info!("Link is up!");

        info!("waiting for stack to be up...");
        self.stack.wait_config_up().await;
        info!("Stack is up!");

        Ok(())
    }
}
