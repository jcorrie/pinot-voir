#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use defmt::*;
use embassy_executor::Executor;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint};
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::multicore::{spawn_core1, Stack};
use embassy_rp::peripherals::DMA_CH1;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use pinot_voir::common::adc_microphone::{adc_task, AudioBlock, AUDIO_BUFFER_SIZE};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::StaticCell;

bind_interrupts!(struct AudioIrqs {
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH1>;
});

static WIFI_CORE: StaticCell<Mutex<CriticalSectionRawMutex, pinot_voir::common::wifi::EmbassyPicoWifiCore>> = StaticCell::new();
use {defmt_rtt as _, panic_probe as _};

// ---------- Executors / Core stacks ----------
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// ---------- Audio channel ----------
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        ENV.init(EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // Spawn Core1 ADC task
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                spawner.spawn(adc_task(&AUDIO_CHANNEL, p.ADC, dma::Channel::new(p.DMA_CH1, AudioIrqs), p.PIN_26).unwrap());
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
        p.DMA_CH2,
        spawner,
        environment_variables,
    )
    .await;

    let shared_wifi_core: SharedEmbassyWifiPicoCore =
        SharedEmbassyWifiPicoCore(WIFI_CORE.init(Mutex::new(embassy_pico_wifi_core)));

    // ---------- Spawn UDP task ----------
    let target_ip = IpAddress::v4(255, 255, 255, 255);
    let port = 1234;
    spawner.spawn(udp_tx_task(
        &AUDIO_CHANNEL,
        shared_wifi_core,
        target_ip,
        port,
    ).unwrap());
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

    let mut stats_timer = Instant::now();
    let mut blocks_ok = 0u32;
    let mut blocks_err = 0u32;
    
    // Calculate the exact timing for each audio block
    // 512 samples at 44100 Hz = 11.61ms per block
    const SAMPLE_RATE_HZ: u32 = 44100;
    const BLOCK_DURATION_MICROS: u64 = (AUDIO_BUFFER_SIZE as u64 * 1_000_000) / SAMPLE_RATE_HZ as u64;
    
    info!("UDP task: Block duration = {} microseconds", BLOCK_DURATION_MICROS);

    loop {
        let send_start = Instant::now();
        
        // Get the latest block, dropping any that have queued up
        let mut block: AudioBlock = audio_channel.receive().await;
        let mut dropped_count = 0;
        
        // Drop excess blocks to maintain real-time performance
        while let Ok(newer_block) = audio_channel.try_receive() {
            block = newer_block;
            dropped_count += 1;
        }
        
        if dropped_count > 0 {
            info!("Dropped {} audio blocks to maintain real-time", dropped_count);
        }
        
        let samples = block.centre_samples();
        let bytes: &[u8] = bytemuck::cast_slice(&samples);

        // Send the audio block
        match socket.send_to(bytes, endpoint).await {
            Ok(_) => blocks_ok += 1,
            Err(e) => {
                blocks_err += 1;
                info!("UDP send error: {:?}", e);
            }
        }

        // Calculate how long we spent processing and sending
        let processing_time = send_start.elapsed();
        
        // Wait for the remainder of the block period
        if processing_time.as_micros() < BLOCK_DURATION_MICROS {
            let wait_time = BLOCK_DURATION_MICROS - processing_time.as_micros();
            Timer::after_micros(wait_time).await;
        } else {
            // If processing took longer than expected, log a warning but don't wait
            info!("Processing took {}μs, expected {}μs", processing_time.as_micros(), BLOCK_DURATION_MICROS);
        }

        // Stats reporting
        if stats_timer.elapsed() >= Duration::from_secs(2) {
            let total = blocks_ok + blocks_err;
            let pct = if total == 0 {
                100.0
            } else {
                (blocks_ok as f32 / total as f32) * 100.0
            };
            info!(
                "UDP Stats: {} ok, {} err ({}% ok), last processing time: {}μs", 
                blocks_ok,
                blocks_err,
                pct,
                processing_time.as_micros()
            );
            stats_timer = Instant::now();
        }
    }
}