//! Microphone-only UDP audio: I2S mic → UDP. Same protocol and transport
//! as `audio_duplex.rs`, just without a speaker attached — received audio
//! packets still refresh the peer endpoint but are never played.
//!
//! Start `py-client/audio_udp.py` on the desktop first (with `--listen-only`
//! if you don't want to send audio); the pico streams back to whoever
//! talks to it.
//!
//! Wiring (SPH0645, SELECT → GND):
//!   bclk : GPIO 18 (physical pin 24)
//!   lrc  : GPIO 19 (physical pin 25)
//!   dout : GPIO 20 (physical pin 26)

#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, PIO1};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use pinot_voir::common::audio::{MicChannel, SpeakerChannel, SAMPLE_RATE};
use pinot_voir::common::audio_udp::{audio_duplex_task, AUDIO_PORT};
use pinot_voir::common::i2s_microphone::{
    i2s_mic_task, Sph0645I2sIn, Sph0645InProgram, USE_ONBOARD_PULLDOWN,
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

static MIC_CHANNEL: MicChannel = MicChannel::new();
// Unused (no speaker attached) but the duplex task needs somewhere to queue
// any audio the peer sends.
static SPEAKER_CHANNEL: SpeakerChannel = SpeakerChannel::new();

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();
static WIFI_CORE: StaticCell<Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>> =
    StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        ENV.init(EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // I2S is DMA-driven (PIO1/DMA_CH1) and WiFi uses PIO0/DMA_CH0/CH2, so
    // there is no resource conflict. Running on the same core eliminates the
    // cross-core wakeup latency that caused ~60 ms recv delays.
    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO1, Irqs);
    let program = Sph0645InProgram::new(&mut common);
    let i2s = Sph0645I2sIn::new(
        &mut common,
        sm0,
        p.DMA_CH1,
        Irqs,
        USE_ONBOARD_PULLDOWN,
        p.PIN_20, // data  (DOUT → GPIO20, physical pin 26)
        p.PIN_18, // bit clock (BCLK → GPIO18, physical pin 24)
        p.PIN_19, // LR clock (LRCLK → GPIO19, physical pin 25)
        SAMPLE_RATE,
        &program,
    );

    spawner.spawn(i2s_mic_task(&MIC_CHANNEL, i2s).unwrap());

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

    spawner.spawn(
        audio_duplex_task(&MIC_CHANNEL, &SPEAKER_CHANNEL, shared_wifi_core, AUDIO_PORT).unwrap(),
    );
}
