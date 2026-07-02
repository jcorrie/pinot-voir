//! Full-duplex UDP audio over WiFi.
//!
//! I2S microphone → UDP out, UDP in → I2S DAC/amp, both at 48 kHz mono
//! 16-bit in 720-sample blocks. See `common/audio.rs` for the wire format
//! and `py-client/audio_udp.py` for the matching desktop client — start the
//! client first; the pico learns where to send from the client's packets.
//!
//! Wiring:
//!   Microphone (SPH0645, SELECT → GND):
//!     bclk : GPIO 18 (physical pin 24)
//!     lrc  : GPIO 19 (physical pin 25)
//!     dout : GPIO 20 (physical pin 26)
//!   Speaker (MAX98357A or similar):
//!     din  : GPIO 13 (physical pin 17)
//!     bclk : GPIO 14 (physical pin 19)
//!     lrc  : GPIO 15 (physical pin 20)
//!
//! Resource map: WiFi uses PIO0 + DMA_CH0/CH2, mic uses PIO1 sm0 + DMA_CH1,
//! speaker uses PIO1 sm1 + DMA_CH3 — no conflicts, everything on Core0.

#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, DMA_CH3, PIO1};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sIn, PioI2sInProgram, PioI2sOut, PioI2sOutProgram};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use pinot_voir::common::audio::{MicChannel, SpeakerChannel, SAMPLE_RATE};
use pinot_voir::common::audio_udp::{audio_duplex_task, AUDIO_PORT};
use pinot_voir::common::i2s_microphone::{
    i2s_mic_task, BIT_DEPTH, CHANNELS, USE_ONBOARD_PULLDOWN,
};
use pinot_voir::common::i2s_speaker::{self, i2s_speaker_task};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::{EmbassyPicoWifiCore, SharedEmbassyWifiPicoCore};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// All interrupts used by this binary in one place — avoids multiply-defined symbols with LTO.
bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
    DMA_IRQ_0  => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>, dma::InterruptHandler<DMA_CH2>, dma::InterruptHandler<DMA_CH3>;
});

static MIC_CHANNEL: MicChannel = MicChannel::new();
static SPEAKER_CHANNEL: SpeakerChannel = SpeakerChannel::new();

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();
static WIFI_CORE: StaticCell<Mutex<CriticalSectionRawMutex, EmbassyPicoWifiCore>> =
    StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let environment_variables: &'static EnvironmentVariables =
        ENV.init(EnvironmentVariables::new());

    let p = embassy_rp::init(Default::default());

    // PIO1 hosts both I2S state machines; WiFi owns PIO0.
    let Pio {
        mut common,
        sm0,
        sm1,
        ..
    } = Pio::new(p.PIO1, Irqs);

    let in_program = PioI2sInProgram::new(&mut common);
    let mic = PioI2sIn::new(
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
        &in_program,
    );

    let out_program = PioI2sOutProgram::new(&mut common);
    let speaker = PioI2sOut::new(
        &mut common,
        sm1,
        p.DMA_CH3,
        Irqs,
        p.PIN_13, // data
        p.PIN_14, // bit clock
        p.PIN_15, // LR clock
        SAMPLE_RATE,
        i2s_speaker::BIT_DEPTH,
        &out_program,
    );

    spawner.spawn(i2s_mic_task(&MIC_CHANNEL, mic).unwrap());
    spawner.spawn(i2s_speaker_task(&SPEAKER_CHANNEL, speaker).unwrap());

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
