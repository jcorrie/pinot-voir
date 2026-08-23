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
use embassy_futures::select::{select, select3, Either, Either3};
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

/// Whether a button is actually fitted to GPIO 22.
///
/// With no button the pin's pull-up reads high forever, which the PTT logic
/// reads as "not talking" — so the microphone never transmits at all and the
/// device is silently listen-only. Setting this false takes the button out of
/// the picture: the microphone is always live and the room is always played.
///
/// That is full duplex, which the half-duplex design exists to avoid. There is
/// no echo canceller here, so if the microphone can hear the speaker they will
/// feed back and everyone else will hear themselves. Fine on a bare board or
/// with headphones; fit the button before putting both in one enclosure.
const PTT_WIRED: bool = false;

/// Is the microphone on the wire? Always, unless a button says otherwise.
fn mic_live() -> bool {
    !PTT_WIRED || intercom::ptt_held()
}

/// Should incoming room audio be discarded rather than played? Only ever while
/// a real button is held — the half-duplex gate needs something to gate on.
fn playback_muted() -> bool {
    PTT_WIRED && intercom::ptt_held()
}

/// How much of [`net_task`] to run. A bisect knob for the hang that stops every
/// task a few hundred ms after the network task starts; see the ladder there.
/// 0 = bound socket only, 1 = plus send, 2 = plus receive, 3 = the real task.
const NET_TASK_STAGE: u8 = 3;

/// Capture gain, in bits. 5 is 32x, which puts conversational speech near
/// -20 dBFS instead of -50. Raise it if voices are still thin; if `pp` in the
/// diagnostic starts pinning at 65535 it is too high and is clipping.
const MIC_GAIN_SHIFT: u32 = 5;
/// Time constant of the DC blocker, in bits. 11 is ~43 ms at 48 kHz: slow
/// enough to leave the lowest voice frequencies alone, fast enough to settle
/// well before anyone finishes pressing the button.
const MIC_DC_SHIFT: u32 = 11;

/// Depth 8 — 160 ms of frames.
///
/// This was 2, on the reasoning that the network task drains it on the same
/// executor and so only has to cover one block of scheduling jitter. That
/// underestimates the drain: it ends in `send_to`, which awaits a transfer over
/// the cyw43 SPI link, and that regularly takes longer than the 20 ms between
/// frames. Two slots meant any send over 40 ms threw a frame away, which is
/// what "choppy" sounds like. The extra depth is only touched when the radio
/// falls behind; when it keeps up the queue still runs one deep and adds no
/// latency.
static MIC: Channel<CriticalSectionRawMutex, Frame, 8> = Channel::new();

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
    if PTT_WIRED {
        let ptt = Input::new(p.PIN_22, Pull::Up);
        intercom::set_ptt(ptt.is_low());
        spawner.spawn(unwrap!(ptt_task(ptt)));
    } else {
        info!("intercom: no PTT button fitted, microphone always live (full duplex, no echo canceller)");
    }

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

    // `main` must not return. It still owns `common` and the two loaded PIO
    // programs, and `Common`'s drop runs `on_pio_drop`, which hands every pin
    // the block was using back to NULL funcsel once the last handle goes. The
    // state machines would keep running with their clocks and data lines
    // disconnected: DMA that never completes, tasks parked forever, and no
    // fault anywhere for a debugger to catch. Holding the handles here for the
    // life of the program is what keeps the I2S pins wired to PIO1.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
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
    let mut blocks: u32 = 0;
    let mut dc_acc: i32 = 0;

    i2s.start();
    info!("intercom: microphone running at {} Hz", I2S_SAMPLE_RATE);

    loop {
        // Queue the next transfer first, then process the block that just
        // landed while the DMA is busy.
        let transfer = i2s.read(front);
        blocks = blocks.wrapping_add(1);

        // Block DC, then lift the level. Both are needed and the order matters.
        //
        // The SPH0645 reaches full scale at 120 dB SPL, so speech at arm's
        // length (~70 dB SPL) sits 50 dB down — about ±100 once the 24-bit
        // sample is scaled to 16 bits. Sent as-is it is technically present and
        // audibly nothing. The same part also carries a DC offset near -1800 at
        // this scale, seventeen times larger than the speech riding on it, and
        // the decimator's FIR has unity DC gain so it would go straight out onto
        // the wire. Applying gain first would just saturate on the offset.
        for (sample, frame) in wide.iter_mut().zip(back.chunks_exact(WORDS_PER_FRAME)) {
            let raw = sample_from_frame(frame) as i32;
            // One-pole running mean, time constant 2^MIC_DC_SHIFT samples —
            // about 21 ms at 48 kHz, well below any voice frequency.
            dc_acc += raw - (dc_acc >> MIC_DC_SHIFT);
            let ac = raw - (dc_acc >> MIC_DC_SHIFT);
            *sample = (ac << MIC_GAIN_SHIFT).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        decimator.process(&wide, &mut frame);

        // Wiring diagnostic every 2 s, carried over from the duplex-audio
        // branch, where the microphone was never confirmed working:
        //   L=00000000, dc=0 pp=0    data line dead — check 3V3, GND, DOUT→GPIO20
        //   L=00000000, R non-zero   mic is on the right channel; tie SELECT to GND
        //   L=8000xxxx, pp=0         stuck MSB — BCLK out of the mic's range
        //   pp rises when you talk   healthy
        //
        // `pp` is peak-to-peak of the decoded samples, which is the only one of
        // these numbers that answers "is it hearing anything". Measuring the raw
        // words instead does not: they are two's complement in the top 16 bits,
        // so an unsigned max ranks every negative sample above every positive
        // one and reports the DC offset. This part has a large one — a quiet
        // room sits near -1800 — so that reading drifts a little block to block
        // and looks convincingly like a live signal while telling you nothing.
        if blocks.is_multiple_of(100) {
            let mut sum: i32 = 0;
            let mut low = i16::MAX;
            let mut high = i16::MIN;
            let mut right_bits = 0u32;
            for pair in back.chunks_exact(WORDS_PER_FRAME) {
                let sample = sample_from_frame(pair);
                sum += sample as i32;
                low = low.min(sample);
                high = high.max(sample);
                right_bits |= pair[1];
            }
            // `out` is the same measurement after DC blocking and gain, which is
            // what actually reaches the wire. Raw `pp` says the microphone hears
            // something; `out` says whether anyone on the other end will.
            let mut out_low = i16::MAX;
            let mut out_high = i16::MIN;
            for sample in wide.iter() {
                out_low = out_low.min(*sample);
                out_high = out_high.max(*sample);
            }
            info!(
                "intercom: i2s raw L[0]={:08x} R[0]={:08x} | dc={} pp={} out={} right={}",
                back[0],
                back[1],
                sum / I2S_FRAME_SAMPLES as i32,
                high as i32 - low as i32,
                out_high as i32 - out_low as i32,
                if right_bits == 0 { "silent" } else { "active" }
            );
        }

        if mic_live() && MIC.try_send(frame).is_err() {
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

        let muted = playback_muted();
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
///
/// Which means joining is not something this end can confirm. UDP has no
/// connection to fail, so `send_to` succeeds whether or not anything is
/// listening, and the logs below are careful to claim only what has actually
/// been observed: datagrams sent, and datagrams received.
#[embassy_executor::task]
async fn net_task(stack: Stack<'static>, server: core::net::Ipv4Addr) -> ! {
    // 640 bytes per frame, so 2048 held only three of them on the way out and
    // `send_to` blocked as soon as the radio fell a frame or two behind —
    // stalling the drain of MIC and dropping capture. Six frames each way.
    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0u8; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0u8; 4096];

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
    info!(
        "intercom: sending to {}:{}, nothing heard back yet (net stage {})",
        server, UDP_PORT, NET_TASK_STAGE
    );

    // Bisect ladder for the executor hang. Every task stops within a few
    // hundred ms of this point, with no fault for probe-rs to catch, so the
    // question is which part of the work below triggers it. Each stage adds one
    // thing and then loops forever, printing a heartbeat: whichever stage stops
    // printing is the one that does it. Set the stage, flash, watch for two or
    // three heartbeats past where it used to die.
    if NET_TASK_STAGE == 0 {
        // Socket exists and is bound, but no traffic at all. If even this hangs,
        // `net_task` is not the trigger and the timing has been a coincidence.
        let mut beats: u32 = 0;
        loop {
            Timer::after(KEEPALIVE_INTERVAL).await;
            beats = beats.wrapping_add(1);
            info!("intercom: stage 0 alive, {} beats", beats);
        }
    }

    if NET_TASK_STAGE == 1 {
        // Transmit only.
        let mut beats: u32 = 0;
        loop {
            Timer::after(KEEPALIVE_INTERVAL).await;
            beats = beats.wrapping_add(1);
            match socket.send_to(&[], room).await {
                Ok(()) => info!("intercom: stage 1 sent keepalive {}", beats),
                Err(e) => warn!("intercom: stage 1 send failed: {:?}", e),
            }
        }
    }

    if NET_TASK_STAGE == 2 {
        // Transmit plus receive, but still no channel from the mic.
        let mut beats: u32 = 0;
        let mut packet = [0u8; 1500];
        let mut keepalive = Ticker::every(KEEPALIVE_INTERVAL);
        loop {
            match select(socket.recv_from(&mut packet), keepalive.next()).await {
                Either::First(Ok((n, from))) => info!("intercom: stage 2 rx {} from {}", n, from),
                Either::First(Err(e)) => warn!("intercom: stage 2 recv failed: {:?}", e),
                Either::Second(()) => {
                    beats = beats.wrapping_add(1);
                    match socket.send_to(&[], room).await {
                        Ok(()) => info!("intercom: stage 2 sent keepalive {}", beats),
                        Err(e) => warn!("intercom: stage 2 send failed: {:?}", e),
                    }
                }
            }
        }
    }

    let mut keepalive = Ticker::every(KEEPALIVE_INTERVAL);
    let mut packet = [0u8; 1500];

    let mut sent: u32 = 0;
    let mut received: u32 = 0;
    let mut errors: u32 = 0;
    let mut answered = false;
    let opened = Instant::now();
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
                // The first datagram back is the only evidence this end ever
                // gets that the room exists. There is no handshake to wait on,
                // so this is the line that means what "joined" used to claim.
                if !answered {
                    answered = true;
                    info!("intercom: room at {}:{} answered", server, UDP_PORT);
                }
                if !playback_muted() {
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
                if !mic_live() && socket.send_to(&[], room).await.is_err() {
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
            // A quiet room and a dead server look identical from here — the
            // server never transmits silence, so receiving nothing is the
            // expected state when no one is talking. Say what is true and let
            // the reader draw the conclusion, rather than picking one.
            if !answered {
                warn!(
                    "intercom: nothing received from {}:{} in {} s — either the room is silent or nothing is listening on that port",
                    server,
                    UDP_PORT,
                    opened.elapsed().as_secs()
                );
            }
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
        let held = mic_live();
        if held != shown {
            wifi.control.gpio_set(0, held).await;
            shown = held;
        }
        Timer::after(Duration::from_millis(25)).await;
    }
}
