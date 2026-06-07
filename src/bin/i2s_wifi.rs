#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use defmt::*;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, PIO1};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sIn, PioI2sInProgram};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use pinot_voir::common::audio::AudioBlock;
use pinot_voir::common::i2s_microphone::{
    i2s_mic_task, BIT_DEPTH, BUFFER_SIZE, CHANNELS, SAMPLE_RATE, USE_ONBOARD_PULLDOWN,
};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// All interrupts used by this binary in one place — avoids multiply-defined symbols with LTO.
bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
    DMA_IRQ_0  => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>, dma::InterruptHandler<DMA_CH2>;
});

// ---------- Audio channel ----------
// Capacity 4: I2S and UDP tx both run on Core0, so no cross-core wakeup delay.
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();
static WIFI_CORE: StaticCell<Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>> =
    StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        ENV.init(EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // Build the I2S driver — both I2S and WiFi tasks run on Core0's executor.
    // I2S is DMA-driven (PIO1/DMA_CH1) and WiFi uses PIO0/DMA_CH0/CH2, so
    // there is no resource conflict.  Running on the same core eliminates the
    // cross-core wakeup latency that caused ~60 ms recv delays.
    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO1, Irqs);
    let program = PioI2sInProgram::new(&mut common);
    let i2s = PioI2sIn::new(
        &mut common,
        sm0,
        p.DMA_CH1,
        Irqs,
        USE_ONBOARD_PULLDOWN,
        p.PIN_20, // data
        p.PIN_18, // bit clock
        p.PIN_19, // LR clock
        SAMPLE_RATE,
        BIT_DEPTH,
        CHANNELS,
        &program,
    );

    // Spawn I2S capture on Core0 before WiFi init.
    spawner.spawn(i2s_mic_task(&AUDIO_CHANNEL, i2s).unwrap());

    // WiFi init (also Core0).
    let embassy_pico_wifi_core = EmbassyPicoWifiCore::connect_to_network(
        p.PIN_23,
        p.PIN_24,
        p.PIN_25,
        p.PIN_29,
        p.PIO0,
        dma::Channel::new(p.DMA_CH0, Irqs),
        dma::Channel::new(p.DMA_CH2, Irqs),
        spawner,
        environment_variables,
    )
    .await;

    let shared_wifi_core: SharedEmbassyWifiPicoCore =
        SharedEmbassyWifiPicoCore(WIFI_CORE.init(Mutex::new(embassy_pico_wifi_core)));

    let target_ip = IpAddress::v4(255, 255, 255, 255);
    let port = 1234;

    spawner.spawn(udp_tx_task(&AUDIO_CHANNEL, shared_wifi_core, target_ip, port).unwrap());
}

#[embassy_executor::task]
async fn udp_tx_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    target_ip: IpAddress,
    port: u16,
) -> ! {
    info!("About to start udp");
    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 2048];
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

    let mut stats_timer = Instant::now();
    let mut blocks_ok = 0u32;
    let mut blocks_err = 0u32;

    const BLOCK_DURATION_MICROS: u64 = (BUFFER_SIZE as u64 * 1_000_000) / SAMPLE_RATE as u64;
    info!("UDP task: block duration = {} µs", BLOCK_DURATION_MICROS);

    loop {
        let t0 = Instant::now();

        let mut block: AudioBlock = audio_channel.receive().await;
        let mut dropped = 0u32;
        while let Ok(newer) = audio_channel.try_receive() {
            block = newer;
            dropped += 1;
        }
        if dropped > 0 {
            info!("Dropped {} stale audio blocks", dropped);
        }

        let t1 = Instant::now();
        let bytes: &[u8] = bytemuck::cast_slice(&block.samples);
        match socket.send_to(bytes, endpoint).await {
            Ok(_) => blocks_ok += 1,
            Err(e) => {
                blocks_err += 1;
                info!("UDP send error: {:?}", e);
            }
        }
        let t2 = Instant::now();

        let recv_us = t1.duration_since(t0).as_micros();
        let send_us = t2.duration_since(t1).as_micros();
        let elapsed = t2.duration_since(t0).as_micros();

        if elapsed < BLOCK_DURATION_MICROS {
            Timer::after_micros(BLOCK_DURATION_MICROS - elapsed).await;
        } else {
            info!(
                "recv={}µs send={}µs total={}µs (expected {}µs)",
                recv_us, send_us, elapsed, BLOCK_DURATION_MICROS
            );
        }

        if stats_timer.elapsed() >= Duration::from_secs(2) {
            let total = blocks_ok + blocks_err;
            let pct = if total == 0 {
                100.0
            } else {
                (blocks_ok as f32 / total as f32) * 100.0
            };
            info!("UDP: {} ok, {} err ({}% ok)", blocks_ok, blocks_err, pct);
            stats_timer = Instant::now();
        }
    }
}
