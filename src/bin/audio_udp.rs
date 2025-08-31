#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use defmt::*;
use embassy_executor::Executor;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::adc::InterruptHandler as ADCInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{ADC, DMA_CH0, PIN_26};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use pinot_voir::common::adc_microphone::{AudioBlock, adc_task};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::StaticCell;
use static_cell::make_static;
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
        make_static!(EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // Spawn Core1 ADC task
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                unwrap!(spawner.spawn(adc_task(&AUDIO_CHANNEL, p.ADC, p.DMA_CH1, p.PIN_26)));
            });
        },
    );

    // ---------- Core0: Connect Wi-Fi asynchronously ----------
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

    let shared_wifi_core: SharedEmbassyWifiPicoCore =
        SharedEmbassyWifiPicoCore(make_static!(Mutex::new(embassy_pico_wifi_core)));

    // ---------- Spawn UDP task ----------
    let target_ip = IpAddress::v4(255, 255, 255, 255);
    let port = 1234;
    unwrap!(spawner.spawn(udp_tx_task(
        &AUDIO_CHANNEL,
        shared_wifi_core,
        target_ip,
        port,
    )));
}

/// Task running on Core0: reads AudioBlocks and sends via UDP
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

    const MAX_UDP_PAYLOAD: usize = 1024;
    let mut stats_timer = Instant::now();
    let mut blocks_ok = 0u32;
    let mut blocks_err = 0u32;

    loop {
        let block: AudioBlock = audio_channel.receive().await;
        let samples = block.centre_samples();
        let bytes: &[u8] = bytemuck::cast_slice(&samples);

        // Send in chunks
        for chunk in bytes.chunks(MAX_UDP_PAYLOAD) {
            match socket.send_to(chunk, endpoint).await {
                Ok(_) => blocks_ok += 1,
                Err(e) => {
                    blocks_err += 1;
                    info!("UDP send error: {:?}", e);
                }
            }
            Timer::after_micros(100).await;
        }

        if stats_timer.elapsed() >= Duration::from_secs(2) {
            let total = blocks_ok + blocks_err;
            let pct = if total == 0 {
                100.0
            } else {
                (blocks_ok as f32 / total as f32) * 100.0
            };
            info!(
                "UDP Stats: {} ok, {} err ({}% ok), sample preview: {}",
                blocks_ok,
                blocks_err,
                pct,
                &bytes[..core::cmp::min(16, bytes.len())]
            );
            stats_timer = Instant::now();
        }
    }
}
