//! Duplex UDP audio transport.
//!
//! One task owns the socket and shuttles audio both ways:
//!
//! * microphone blocks arriving on the [`MicChannel`] are serialised
//!   (see [`crate::common::audio`] for the wire format) and sent to the
//!   current peer;
//! * received packets are validated and queued on the [`SpeakerChannel`]
//!   for the I2S output task.
//!
//! The pico never initiates. It binds [`AUDIO_PORT`] and waits; the client
//! sends audio (or header-only keep-alives) first, and the pico learns the
//! return endpoint from the packet's source address. If nothing is heard
//! for [`PEER_TIMEOUT`] the peer is forgotten and the mic stream pauses.
//! This replaces the old broadcast-to-255.255.255.255 scheme, which both
//! saturated the AP's low broadcast rate and made a return path impossible.
//!
//! There is deliberately no send pacing here: the I2S capture DMA already
//! produces exactly one block per block period, so the channel itself is
//! the clock. Pacing again in this task (as the old `udp_tx_task` did)
//! only lets jitter accumulate until blocks get dropped.

use defmt::*;
use embassy_futures::select::{select, Either};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::IpEndpoint;
use embassy_time::{Duration, Instant};

use crate::common::audio::{
    parse_packet, write_packet, AudioBlock, Direction, MicChannel, SpeakerChannel, PACKET_BYTES,
};
use crate::common::wifi::SharedEmbassyWifiPicoCore;

pub const AUDIO_PORT: u16 = 1234;

/// Forget the peer after this long without hearing from it.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(3);

const STATS_INTERVAL: Duration = Duration::from_secs(5);

#[embassy_executor::task]
pub async fn audio_duplex_task(
    mic_channel: &'static MicChannel,
    speaker_channel: &'static SpeakerChannel,
    shared_wifi_core: SharedEmbassyWifiPicoCore,
    port: u16,
) -> ! {
    // Socket buffers sized for a few packets in flight each way; the old
    // 1024-byte rx buffer couldn't hold even one 1452-byte packet.
    let mut rx_buffer = [0u8; PACKET_BYTES * 4];
    let mut tx_buffer = [0u8; PACKET_BYTES * 4];
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];

    let stack = shared_wifi_core.0.lock().await.stack;
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(port).expect("UDP bind failed");
    info!("Audio duplex: listening on UDP port {}", port);

    let mut rx_pkt = [0u8; PACKET_BYTES];
    let mut tx_pkt = [0u8; PACKET_BYTES];
    let mut tx_seq = 0u32;
    let mut peer: Option<IpEndpoint> = None;
    let mut last_heard = Instant::now();

    let mut tx_ok = 0u32;
    let mut tx_err = 0u32;
    let mut rx_ok = 0u32;
    let mut rx_dropped = 0u32;
    let mut rx_bad = 0u32;
    let mut stats_timer = Instant::now();

    loop {
        match select(mic_channel.receive(), socket.recv_from(&mut rx_pkt)).await {
            Either::First(block) => {
                if peer.is_some() && last_heard.elapsed() > PEER_TIMEOUT {
                    info!("Audio peer timed out");
                    peer = None;
                }
                // With no peer the block is simply discarded; capture keeps
                // running so the stream resumes instantly when one appears.
                if let Some(endpoint) = peer {
                    write_packet(&mut tx_pkt, Direction::FromPico, tx_seq, &block.samples);
                    tx_seq = tx_seq.wrapping_add(1);
                    match socket.send_to(&tx_pkt, endpoint).await {
                        Ok(()) => tx_ok += 1,
                        Err(e) => {
                            tx_err += 1;
                            warn!("UDP send error: {:?}", e);
                        }
                    }
                }
            }
            Either::Second(Ok((len, meta))) => match parse_packet(&rx_pkt[..len], Direction::ToPico)
            {
                Some((seq, payload)) => {
                    if peer != Some(meta.endpoint) {
                        info!("Audio peer: {:?}", meta.endpoint);
                        peer = Some(meta.endpoint);
                    }
                    last_heard = Instant::now();
                    if !payload.is_empty() {
                        let mut block = AudioBlock::new();
                        block.block_id = seq;
                        block.timestamp = Instant::now().as_micros();
                        block.update_samples_from_le_bytes(payload);
                        // Keep playback latency bounded: when the jitter
                        // buffer is full, drop the oldest block, not the new one.
                        if speaker_channel.try_send(block).is_err() {
                            let _ = speaker_channel.try_receive();
                            let _ = speaker_channel.try_send(block);
                            rx_dropped += 1;
                        }
                        rx_ok += 1;
                    }
                }
                None => rx_bad += 1,
            },
            Either::Second(Err(e)) => {
                warn!("UDP recv error: {:?}", e);
            }
        }

        if stats_timer.elapsed() >= STATS_INTERVAL {
            info!(
                "Audio UDP: tx {} ok / {} err, rx {} ok / {} dropped / {} bad",
                tx_ok, tx_err, rx_ok, rx_dropped, rx_bad
            );
            stats_timer = Instant::now();
        }
    }
}
