"""Desktop stand-in for the SPQR audio room, for bench-testing the pico.

The real room lives in SPQR (`src/functions/stream_audio.rs`). This is the
smallest thing that speaks the same protocol, so the pico can be brought up
without running the server: it binds the room's UDP port, plays whatever the
pico sends, and sends your microphone back. With exactly two participants,
"the sum of everyone else" is just the other one, so this is faithful enough
to prove out the firmware end to end.

Wire format (see SPQR `docs/audio-protocol.md`) — raw PCM16 LE, mono, 16 kHz,
320 samples per 20 ms frame, 640 bytes, no header. A zero-length datagram is a
listener registering itself without contributing audio.

Two behaviours here exist to exercise firmware paths rather than to be
convenient:

* Silence is not transmitted. Frames below --squelch are dropped, exactly as
  the server drops all-zero frames, so the pico's playback path gets the gaps
  it has to survive.
* The peer is forgotten after 5 s of silence, same as the server, so a device
  that stops sending really does have to keep itself registered.

Usage:
    uv run audio_room.py                      # duplex: play the pico, send your mic
    uv run audio_room.py --listen-only        # play the pico, send nothing
    uv run audio_room.py --list-devices
    uv run audio_room.py --input-device 1 --output-device 3
"""

import argparse
import os
import queue
import socket
import sys
import threading
import time

import numpy as np
import sounddevice as sd

SAMPLE_RATE = 16_000
FRAME_SAMPLES = 320
FRAME_BYTES = FRAME_SAMPLES * 2
UDP_PORT = 1234
PEER_TIMEOUT = 5.0
# Depth of the playback queue. Same trade the pico makes: a couple of frames of
# slack, then drop, because late audio is worth nothing.
QUEUE_FRAMES = 8


class Peer:
    """The device we are talking to, learned from whatever arrives."""

    def __init__(self):
        self.lock = threading.Lock()
        self.addr = None
        self.last_seen = 0.0

    def touch(self, addr):
        with self.lock:
            if addr != self.addr:
                print(f"peer joined: {addr[0]}:{addr[1]}")
            self.addr = addr
            self.last_seen = time.time()

    def current(self):
        with self.lock:
            if self.addr and time.time() - self.last_seen > PEER_TIMEOUT:
                print(f"peer timed out: {self.addr[0]}:{self.addr[1]}")
                self.addr = None
            return self.addr


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.rx = 0
        self.rx_keepalive = 0
        self.rx_bad = 0
        self.tx = 0
        self.tx_squelched = 0
        self.dropped = 0
        self.peak = 0


def rx_loop(sock, peer, stats, playback):
    """Receive from the pico: register the peer, queue audio for playback."""
    while True:
        try:
            data, addr = sock.recvfrom(4096)
        except socket.timeout:
            continue

        peer.touch(addr)

        if len(data) == 0:
            with stats.lock:
                stats.rx_keepalive += 1
            continue
        if len(data) < FRAME_BYTES:
            with stats.lock:
                stats.rx_bad += 1
            continue

        samples = np.frombuffer(data[:FRAME_BYTES], dtype="<i2")
        with stats.lock:
            stats.rx += 1
            # Widen first: np.abs on int16 -32768 overflows back to itself.
            stats.peak = int(np.max(np.abs(samples.astype(np.int32))))
        try:
            playback.put_nowait(samples)
        except queue.Full:
            with stats.lock:
                stats.dropped += 1


def play_loop(output_device, playback):
    """Drain the playback queue to the speakers, filling gaps with silence."""
    silence = np.zeros(FRAME_SAMPLES, dtype=np.int16)
    with sd.OutputStream(
        device=output_device,
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="int16",
        blocksize=FRAME_SAMPLES,
        latency="low",
    ) as stream:
        while True:
            try:
                # Slightly longer than a frame, so a late packet is waited for
                # but a silent room does not stall the output clock.
                samples = playback.get(timeout=0.05)
            except queue.Empty:
                samples = silence
            stream.write(samples)


def tx_loop(sock, peer, stats, input_device, squelch):
    """Send the desktop microphone to the pico, skipping silence."""
    with sd.InputStream(
        device=input_device,
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="int16",
        blocksize=FRAME_SAMPLES,
        latency="low",
    ) as stream:
        while True:
            samples, _overflowed = stream.read(FRAME_SAMPLES)
            addr = peer.current()
            if addr is None:
                continue
            if int(np.max(np.abs(samples.astype(np.int32)))) < squelch:
                with stats.lock:
                    stats.tx_squelched += 1
                continue
            # Explicitly little-endian: the protocol says so, and `tobytes()`
            # alone would follow the host.
            sock.sendto(samples.astype("<i2").tobytes(), addr)
            with stats.lock:
                stats.tx += 1


def main():
    parser = argparse.ArgumentParser(
        description="Desktop stand-in for the SPQR audio room"
    )
    parser.add_argument("--port", type=int, default=UDP_PORT)
    parser.add_argument(
        "--listen-only",
        action="store_true",
        help="play the pico but send nothing back",
    )
    parser.add_argument(
        "--squelch",
        type=int,
        default=500,
        help="peak below which a frame counts as silence and is not sent "
        "(0 to send everything)",
    )
    parser.add_argument("--input-device", default=None)
    parser.add_argument("--output-device", default=None)
    parser.add_argument("--list-devices", action="store_true")
    args = parser.parse_args()

    if args.list_devices:
        print(sd.query_devices())
        return

    def parse_device(dev):
        if dev is None:
            return None
        return int(dev) if dev.isdigit() else dev

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", args.port))
    sock.settimeout(1.0)

    peer = Peer()
    stats = Stats()
    playback = queue.Queue(maxsize=QUEUE_FRAMES)

    def spawn(target, *target_args):
        """A dead worker means a dead client, so exit loudly rather than hang."""

        def runner():
            try:
                target(*target_args)
            except Exception as e:
                print(f"fatal: {target.__name__}: {e}", file=sys.stderr)
                os._exit(1)

        threading.Thread(target=runner, daemon=True).start()

    spawn(rx_loop, sock, peer, stats, playback)
    spawn(play_loop, parse_device(args.output_device), playback)
    if not args.listen_only:
        spawn(tx_loop, sock, peer, stats, parse_device(args.input_device), args.squelch)

    mode = "listen-only" if args.listen_only else "duplex"
    print(
        f"room ({mode}) on udp 0.0.0.0:{args.port} — "
        f"{FRAME_SAMPLES} samples/frame @ {SAMPLE_RATE} Hz, waiting for the pico"
    )

    last_rx, last_time = 0, time.time()
    try:
        while True:
            time.sleep(2.0)
            with stats.lock:
                now = time.time()
                rate = (stats.rx - last_rx) / (now - last_time)
                last_rx, last_time = stats.rx, now
                print(
                    f"rx={stats.rx} ({rate:.1f}/s) keepalive={stats.rx_keepalive} "
                    f"bad={stats.rx_bad} dropped={stats.dropped} "
                    f"tx={stats.tx} squelched={stats.tx_squelched} peak={stats.peak}"
                )
    except KeyboardInterrupt:
        print("\nbye")
        sys.exit(0)


if __name__ == "__main__":
    main()
