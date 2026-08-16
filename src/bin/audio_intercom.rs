//! Push-to-talk intercom client for the SPQR audio room.
//!
//! Half duplex by design. Holding the PTT button puts the microphone on the
//! wire; releasing it plays the room. The two are never live together — see
//! [`intercom::ptt_held`] for why.
//!
//! # Tasks
//!
//! Everything runs on core 0. An earlier revision of this repo put audio
//! capture on core 1 and measured ~60 ms of cross-core wakeup latency on the
//! receive path; on one executor the I2S DMA completions and the network poll
//! interleave without that cost, and there is compute to spare either way.
//!
//! * `mic_task` — I2S in at 48 kHz, decimated to 320-sample frames. Frames are
//!   only queued while PTT is held.
//! * `speaker_task` — I2S out at 48 kHz, interpolated from the jitter buffer.
//!   Free-running: it emits a block every 20 ms whether or not audio arrived.
//! * `net_task` — one UDP socket for both directions, plus the keepalive that
//!   registers this device as a listener while it has nothing to send.
//! * `ptt_task` — debounced button.
//! * `led_task` — onboard LED mirrors PTT, so you can see when you are live.
//!
//! # Wiring
//!
//! | Signal | GPIO | Pin |
//! |---|---|---|
//! | Mic BCLK | 18 | 24 |
//! | Mic LRCLK | 19 | 25 |
//! | Mic DOUT | 20 | 26 |
//! | DAC DIN | 13 | 17 |
//! | DAC BCLK | 14 | 19 |
//! | DAC LRCLK | 15 | 20 |
//! | PTT button | 22 | 29 |
//!
//! Microphone SELECT to ground. The DAC pins match `audio_duplex` on the
//! duplex-audio branch so the same breadboard serves both.
//!
//! The button shorts GPIO 22 to ground; the internal pull-up does the rest, so
//! it is two wires and no external parts. Bit and word clocks have to be
//! consecutive GPIOs in that order — PIO side-set drives them as one contiguous
//! range.

#![no_std]
#![no_main]

use core::mem;

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Ticker, Timer};
use pinot_voir::common::i2s_microphone::{
    sample_from_frame, Sph0645I2sIn, Sph0645InProgram, USE_ONBOARD_PULLDOWN, WORDS_PER_FRAME,
};
use pinot_voir::common::intercom::{
    self, Frame, FRAME_SAMPLES, KEEPALIVE_INTERVAL, PLAYBACK, UDP_PORT,
};
use pinot_voir::common::irqs::Irqs;
use pinot_voir::common::resample::{Decimator, Interpolator, I2S_FRAME_SAMPLES};
use pinot_voir::common::shared_functions::EnvironmentVariables;
use pinot_voir::common::wifi::EmbassyPicoWifiCore;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

/// I2S bus rate. Three times the 16 kHz wire rate, and inside the range every
/// common I2S MEMS microphone is specified for.
const I2S_SAMPLE_RATE: u32 = 48_000;
/// Bits per channel slot, output side. 32 rather than 16 puts BCLK at
/// 3.072 MHz, matching what the microphone needs on the input side and clearing
/// the 2.2 MHz minimum the MAX98357A asks for. One 32-bit word per channel, so
/// a stereo frame is two words — see [`fill_stereo`].
const I2S_BIT_DEPTH: u32 = 32;

/// Long enough for a tactile switch to settle, short enough not to clip the
/// start of a word.
const PTT_DEBOUNCE: Duration = Duration::from_millis(15);

/// Depth 2: the network task drains this on the same executor, so it only has
/// to cover one block of scheduling jitter.
static MIC: Channel<CriticalSectionRawMutex, Frame, 2> = Channel::new();

static ENV: StaticCell<EnvironmentVariables> = StaticCell::new();
static WIFI: StaticCell<EmbassyPicoWifiCore> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let env: &'static EnvironmentVariables = ENV.init(EnvironmentVariables::new());

    let server: core::net::Ipv4Addr = env
        .audio_server_ip
        .expect("AUDIO_SERVER_IP missing from .env")
        .trim()
        .parse()
        .expect("AUDIO_SERVER_IP is not a dotted-quad IPv4 address");

    // Both I2S state machines share PIO1: SM0 reads the microphone, SM1 drives
    // the DAC. Two programs of eight instructions each fit the 32-word
    // instruction memory with room over.
    let Pio {
        mut common,
        sm0,
        sm1,
        ..
    } = Pio::new(p.PIO1, Irqs);

    let in_program = Sph0645InProgram::new(&mut common);
    let i2s_in = Sph0645I2sIn::new(
        &mut common,
        sm0,
        p.DMA_CH1,
        Irqs,
        USE_ONBOARD_PULLDOWN,
        p.PIN_20, // DOUT
        p.PIN_18, // BCLK
        p.PIN_19, // LRCLK
        I2S_SAMPLE_RATE,
        &in_program,
    );

    let out_program = PioI2sOutProgram::new(&mut common);
    let i2s_out = PioI2sOut::new(
        &mut common,
        sm1,
        p.DMA_CH2,
        Irqs,
        p.PIN_13, // DIN
        p.PIN_14, // BCLK
        p.PIN_15, // LRCLK
        I2S_SAMPLE_RATE,
        I2S_BIT_DEPTH,
        &out_program,
    );

    // Sample the button before anything can transmit, so a device that powers up
    // with PTT held does not start out in the wrong state.
    let ptt = Input::new(p.PIN_22, Pull::Up);
    intercom::set_ptt(ptt.is_low());
    spawner.spawn(unwrap!(ptt_task(ptt)));

    spawner.spawn(unwrap!(mic_task(i2s_in)));
    spawner.spawn(unwrap!(speaker_task(i2s_out)));

    let wifi: &'static mut EmbassyPicoWifiCore = WIFI.init(
        EmbassyPicoWifiCore::connect_to_network(
            p.PIN_23, p.PIN_24, p.PIN_25, p.PIN_29, p.PIO0, p.DMA_CH0, spawner, env,
        )
        .await,
    );
    let stack = wifi.stack;

    spawner.spawn(unwrap!(led_task(wifi)));
    spawner.spawn(unwrap!(net_task(stack, server)));
}

/// Write one mono sample into a stereo frame's pair of 32-bit slots. The PIO
/// output program shifts MSB-first, so a 16-bit sample sits at the top of each
/// word. Driving both channels suits a mono amplifier however its channel
/// select is strapped.
fn fill_stereo(frame: &mut [u32], sample: i16) {
    let word = (sample as u16 as u32) << 16;
    frame[0] = word;
    frame[1] = word;
}

/// Capture, decimate, and queue for transmission while PTT is held.
///
/// The 48 kHz DMA is the transmit clock: 960 samples is exactly 20 ms, so
/// frames leave at the 50 Hz the protocol asks for without a timer to drift
/// against. The decimator runs even when muted, which costs a few percent of a
/// core and means its filter history is warm the instant the button goes down.
#[embassy_executor::task]
async fn mic_task(mut i2s: Sph0645I2sIn<'static, PIO1, 0>) -> ! {
    const WORDS: usize = I2S_FRAME_SAMPLES * WORDS_PER_FRAME;

    static DMA: StaticCell<[u32; WORDS * 2]> = StaticCell::new();
    static DECIMATOR: StaticCell<Decimator> = StaticCell::new();

    let dma = DMA.init([0; WORDS * 2]);
    let (mut back, mut front) = dma.split_at_mut(WORDS);
    let decimator = DECIMATOR.init(Decimator::new());

    let mut wide = [0i16; I2S_FRAME_SAMPLES];
    let mut frame: Frame = [0; FRAME_SAMPLES];
    let mut dropped: u32 = 0;

    i2s.start();
    info!("intercom: microphone running at {} Hz", I2S_SAMPLE_RATE);

    loop {
        // Queue the next transfer first, then process the block that just
        // landed while the DMA is busy.
        let transfer = i2s.read(front);

        for (sample, frame) in wide.iter_mut().zip(back.chunks_exact(WORDS_PER_FRAME)) {
            *sample = sample_from_frame(frame);
        }
        decimator.process(&wide, &mut frame);

        if intercom::ptt_held() && MIC.try_send(frame).is_err() {
            dropped = dropped.wrapping_add(1);
            if dropped.is_multiple_of(50) {
                warn!(
                    "intercom: {} mic frames dropped, network not keeping up",
                    dropped
                );
            }
        }

        transfer.await;
        mem::swap(&mut back, &mut front);
    }
}

/// Play the room, or silence.
///
/// This loop never stalls waiting for audio. The protocol sends nothing at all
/// when the room is quiet, so a block of zeros is the ordinary case and the I2S
/// clock has to keep running through it regardless.
#[embassy_executor::task]
async fn speaker_task(mut i2s: PioI2sOut<'static, PIO1, 1>) -> ! {
    const WORDS: usize = I2S_FRAME_SAMPLES * WORDS_PER_FRAME;

    static DMA: StaticCell<[u32; WORDS * 2]> = StaticCell::new();
    static INTERPOLATOR: StaticCell<Interpolator> = StaticCell::new();

    let dma = DMA.init([0; WORDS * 2]);
    let (mut back, mut front) = dma.split_at_mut(WORDS);
    let interpolator = INTERPOLATOR.init(Interpolator::new());

    let mut wide = [0i16; I2S_FRAME_SAMPLES];
    let mut frame: Frame = [0; FRAME_SAMPLES];
    let mut muted_before = false;

    i2s.start();
    info!("intercom: speaker running at {} Hz", I2S_SAMPLE_RATE);

    loop {
        let transfer = i2s.write(front);

        let muted = intercom::ptt_held();
        let have_audio = if muted {
            // Half-duplex gate. Discard anything banked so releasing the button
            // does not replay audio from while we were talking, and clear the
            // filter so the first frame back is not smeared across the gap.
            if !muted_before {
                PLAYBACK.lock(|p| p.borrow_mut().flush());
                interpolator.reset();
            }
            false
        } else {
            PLAYBACK.lock(|p| p.borrow_mut().pop(&mut frame))
        };
        muted_before = muted;

        if have_audio {
            interpolator.process(&frame, &mut wide);
            for (out_frame, sample) in back.chunks_exact_mut(WORDS_PER_FRAME).zip(wide.iter()) {
                fill_stereo(out_frame, *sample);
            }
        } else {
            back.fill(0);
        }

        transfer.await;
        mem::swap(&mut back, &mut front);
    }
}

/// One socket, both directions.
///
/// The mix comes back to the address a datagram was sent from, so the local
/// port is ephemeral and there is nothing to subscribe to — the first packet
/// out is the join.
#[embassy_executor::task]
async fn net_task(stack: Stack<'static>, server: core::net::Ipv4Addr) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx_buffer = [0u8; 2048];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_buffer = [0u8; 2048];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    // Port 0 asks the stack for an ephemeral one.
    socket.bind(0).expect("intercom: UDP bind failed");

    let room = IpEndpoint::new(IpAddress::Ipv4(server), UDP_PORT);
    info!("intercom: joined room at {}:{}", server, UDP_PORT);

    let mut keepalive = Ticker::every(KEEPALIVE_INTERVAL);
    let mut packet = [0u8; 1500];

    let mut sent: u32 = 0;
    let mut received: u32 = 0;
    let mut errors: u32 = 0;
    let mut stats = Instant::now();

    loop {
        match select3(
            MIC.receive(),
            socket.recv_from(&mut packet),
            keepalive.next(),
        )
        .await
        {
            // Mic frame, already gated on PTT by the capture task.
            Either3::First(frame) => {
                let bytes: &[u8] = bytemuck::cast_slice(&frame);
                match socket.send_to(bytes, room).await {
                    Ok(()) => sent = sent.wrapping_add(1),
                    Err(e) => {
                        errors = errors.wrapping_add(1);
                        debug!("intercom: send failed: {:?}", e);
                    }
                }
            }

            // Room audio. Always drained, even while transmitting, so the
            // socket buffer cannot back up — but only played when we are not.
            Either3::Second(Ok((n, _))) => {
                received = received.wrapping_add(1);
                if !intercom::ptt_held() {
                    PLAYBACK.lock(|p| p.borrow_mut().push(&packet[..n]));
                }
            }
            Either3::Second(Err(e)) => {
                errors = errors.wrapping_add(1);
                debug!("intercom: recv failed: {:?}", e);
            }

            // A zero-length datagram registers us as a listener without
            // contributing audio. Only needed when we are not already sending:
            // any datagram refreshes the server's timeout.
            Either3::Third(()) => {
                if !intercom::ptt_held() && socket.send_to(&[], room).await.is_err() {
                    errors = errors.wrapping_add(1);
                }
            }
        }

        if stats.elapsed() >= Duration::from_secs(5) {
            let (overruns, underruns) =
                PLAYBACK.lock(|p| (p.borrow().overruns, p.borrow().underruns));
            info!(
                "intercom: tx {} rx {} err {} | jitter buffer: {} over, {} under",
                sent, received, errors, overruns, underruns
            );
            stats = Instant::now();
        }
    }
}

/// Debounced push-to-talk.
///
/// Waiting on an edge and then sampling after the contacts settle costs nothing
/// while idle. Bounces during the settling window re-trigger this loop and
/// re-read the same settled level, which is harmless.
#[embassy_executor::task]
async fn ptt_task(mut button: Input<'static>) -> ! {
    loop {
        button.wait_for_any_edge().await;
        Timer::after(PTT_DEBOUNCE).await;
        let held = button.is_low();
        if held != intercom::ptt_held() {
            intercom::set_ptt(held);
            info!("intercom: {}", if held { "talking" } else { "listening" });
        }
    }
}

/// Onboard LED follows PTT. It hangs off the WiFi chip, so each change is an
/// SPI transaction — only write on a transition, not every poll.
#[embassy_executor::task]
async fn led_task(wifi: &'static mut EmbassyPicoWifiCore) -> ! {
    let mut shown = false;
    wifi.control.gpio_set(0, shown).await;
    loop {
        let held = intercom::ptt_held();
        if held != shown {
            wifi.control.gpio_set(0, held).await;
            shown = held;
        }
        Timer::after(Duration::from_millis(25)).await;
    }
}
