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
use embassy_rp::multicore::{spawn_core1, Stack};
use embassy_rp::peripherals::{ADC, DMA_CH0, PIN_26};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use pinot_voir::common::adc_microphone::{adc_task, AudioBlock, AUDIO_BUFFER_SIZE};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::make_static;
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
// ...existing code...

#[embassy_executor::task]
async fn udp_tx_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    target_ip: IpAddress,
    port: u16,
) -> ! {
    // Increase these significantly
    let mut rx_buffer = [0; 2048]; // Was 1024
    let mut tx_buffer = [0; 8192]; // Was 4096
    let mut rx_meta = [PacketMetadata::EMPTY; 32]; // Was 16
    let mut tx_meta = [PacketMetadata::EMPTY; 128]; // Was 64

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
    let mut consecutive_errors = 0u32;
    let mut blocks_dropped = 0u32;

    // Calculate the exact timing for each audio block
    // 512 samples at 44100 Hz = 11.61ms per block
    const SAMPLE_RATE_HZ: u32 = 44100;
    const BLOCK_DURATION_MICROS: u64 =
        (AUDIO_BUFFER_SIZE as u64 * 1_000_000) / SAMPLE_RATE_HZ as u64;

    info!(
        "UDP task: Block duration = {} microseconds",
        BLOCK_DURATION_MICROS
    );

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
            blocks_dropped += dropped_count;
            info!(
                "Dropped {} audio blocks to maintain real-time",
                dropped_count
            );
        }

        // Circuit breaker: if we have too many consecutive errors, skip blocks
        if consecutive_errors > 5 {
            info!("Circuit breaker: skipping block due to network overload");
            blocks_dropped += 1;
            // Wait longer to let network recover
            Timer::after_millis(50).await;
            continue;
        }

        let samples = block.centre_samples();
        let bytes: &[u8] = bytemuck::cast_slice(&samples);

        // Send the audio block with timeout
        match socket.send_to(bytes, endpoint).await {
            Ok(_) => {
                blocks_ok += 1;
                consecutive_errors = 0; // Reset error counter on success
            }
            Err(e) => {
                blocks_err += 1;
                consecutive_errors += 1;
                info!(
                    "UDP send error: {:?} (consecutive: {})",
                    e, consecutive_errors
                );

                // Exponential backoff based on error count
                let backoff_ms = match consecutive_errors {
                    1..=2 => 5,
                    3..=5 => 20,
                    _ => 100,
                };
                Timer::after_millis(backoff_ms).await;
            }
        }

        // Calculate how long we spent processing and sending
        let processing_time = send_start.elapsed();

        // Adaptive rate limiting: slow down if we're having errors
        let target_delay = if consecutive_errors > 0 {
            // Slower rate when having network issues
            BLOCK_DURATION_MICROS * 2
        } else {
            BLOCK_DURATION_MICROS
        };

        // Wait for the remainder of the block period
        if processing_time.as_micros() < target_delay {
            let wait_time = target_delay - processing_time.as_micros();
            Timer::after_micros(wait_time).await;
        } else {
            // If processing took longer than expected, log a warning but don't wait
            info!(
                "Processing took {}μs, expected {}μs",
                processing_time.as_micros(),
                target_delay
            );
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
                "UDP Stats: {} ok, {} err, {} dropped ({}% ok), consecutive errors: {}, last processing time: {}μs",
                blocks_ok,
                blocks_err,
                blocks_dropped,
                pct,
                consecutive_errors,
                processing_time.as_micros()
            );
            stats_timer = Instant::now();
        }
    }
}
