#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Executor;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::multicore::{spawn_core1, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use embedded_rtp::l16::{self, Concealer};
use embedded_rtp::packet::{self, Packet};
use embedded_rtp::profile::{CLOCK_RATE, FRAME_BYTES, FRAME_SAMPLES, PACKET_BYTES, PAYLOAD_TYPE};
use embedded_rtp::Sender;
use pinot_voir::common::adc_microphone::{adc_task, AudioBlock, FRAME, FRAME_TICKS};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// ---------- Executors / Core stacks ----------
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// ---------- Audio channel ----------
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        picoserve::make_static!(EnvironmentVariables, EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // Spawn Core1 ADC task
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                spawner.spawn(defmt::unwrap!(adc_task(
                    &AUDIO_CHANNEL,
                    p.ADC,
                    p.DMA_CH1,
                    p.PIN_26
                )));
            });
        },
    );

    // ---------- Core0: Connect Wi-Fi asynchronously ----------
    let embassy_pico_wifi_core = EmbassyPicoWifiCore::connect_to_network(
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

    let shared_wifi_core: SharedEmbassyWifiPicoCore = SharedEmbassyWifiPicoCore(
        picoserve::make_static!(Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>, Mutex::new(embassy_pico_wifi_core)),
    );

    // ---------- Spawn UDP task ----------
    let target_ip = IpAddress::v4(255, 255, 255, 255);
    let port = 1234;
    spawner.spawn(defmt::unwrap!(udp_tx_task(
        &AUDIO_CHANNEL,
        shared_wifi_core,
        target_ip,
        port,
    )));
}

/// Task running on Core0: reads AudioBlocks and sends via UDP with proper rate limiting
#[embassy_executor::task]
async fn udp_tx_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    target_ip: IpAddress,
    port: u16,
) -> ! {
    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];
    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];

    let endpoint = IpEndpoint::new(target_ip, port);
    let mut socket = UdpSocket::new(
        shared_wifi_core.0.lock().await.stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(port).expect("UDP bind failed");

    let mut sender = Sender::new(0x1234_5678, PAYLOAD_TYPE, FRAME_TICKS, 0, 0);
    let mut next = Instant::now();

    loop {
        let block: AudioBlock = audio_channel.receive().await;
        let mut buf = [0u8; PACKET_BYTES];

        let samples = block.centre_samples();

        l16::encode(&samples, &mut buf[packet::HEADER_LEN..]);
        let len = sender.send_in_place(&mut buf, FRAME_BYTES).unwrap();
        match socket.send_to(&buf[..len], endpoint).await {
            Ok(_) => (),
            Err(e) => {
                info!("UDP send error: {:?}", e);
            }
        }
        next += FRAME;
        Timer::after(next.saturating_duration_since(Instant::now())).await;
    }
}
