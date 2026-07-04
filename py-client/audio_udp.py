"""Duplex UDP audio client for the pico.

Sends microphone audio to the pico and plays back the pico's microphone
stream. The client always initiates: the pico learns where to send from the
source address of the packets (or keep-alives) we send it, so this must be
started pointing at the pico's IP.

Wire format (must match src/common/audio.rs):

    bytes 0..4   magic b"PVAU"
    byte  4      protocol version (1)
    byte  5      direction: 0 = client -> pico, 1 = pico -> client
    bytes 6..8   reserved, zero
    bytes 8..12  sequence number, u32 little-endian
    bytes 12..   payload: BUFFER_SIZE x i16 LE mono samples @ 48 kHz, or empty
                 (a header-only packet is a keep-alive)

Usage:
    python audio_udp.py <pico-ip>                 # full duplex
    python audio_udp.py <pico-ip> --listen-only   # receive only (keep-alives sent)
    python audio_udp.py --list-devices
"""

import argparse
import ipaddress
import os
import socket
import struct
import sys
import threading
import time

import numpy as np
import sounddevice as sd

SAMPLE_RATE = 48_000
# Samples per block, must match src/common/audio.rs. Sized so a packet's IP
# datagram (payload + 12-byte header + 28 bytes UDP/IP) stays under the
# 1280-byte MTU of WireGuard/Tailscale tunnels.
BUFFER_SIZE = 600
MAGIC = b"PVAU"
PROTOCOL_VERSION = 1
DIR_TO_PICO = 0
DIR_FROM_PICO = 1
HEADER = struct.Struct("<4sBBHI")  # magic, version, direction, reserved, seq
PAYLOAD_BYTES = BUFFER_SIZE * 2
PACKET_BYTES = HEADER.size + PAYLOAD_BYTES
BLOCK_SECONDS = BUFFER_SIZE / SAMPLE_RATE
KEEPALIVE_SECONDS = 0.5


def pack(direction: int, seq: int, payload: bytes = b"") -> bytes:
    return HEADER.pack(MAGIC, PROTOCOL_VERSION, direction, 0, seq & 0xFFFFFFFF) + payload


def unpack(data: bytes):
    """Return (seq, payload) for a valid pico->client packet, else None."""
    if len(data) not in (HEADER.size, PACKET_BYTES):
        return None
    magic, version, direction, _, seq = HEADER.unpack_from(data)
    if magic != MAGIC or version != PROTOCOL_VERSION or direction != DIR_FROM_PICO:
        return None
    return seq, data[HEADER.size:]


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.rx = 0
        self.rx_bad = 0
        self.rx_lost = 0
        self.tx = 0
        self.peak = 0


def rx_loop(sock: socket.socket, output_device, stats: Stats):
    """Receive pico audio and play it."""
    last_seq = None
    with sd.OutputStream(
        device=output_device,
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="int16",
        blocksize=BUFFER_SIZE,
        latency="low",
    ) as stream:
        while True:
            try:
                data, _ = sock.recvfrom(4096)
            except socket.timeout:
                last_seq = None
                continue

            parsed = unpack(data)
            if parsed is None:
                with stats.lock:
                    stats.rx_bad += 1
                continue
            seq, payload = parsed
            if not payload:
                continue  # keep-alive

            with stats.lock:
                if last_seq is not None:
                    gap = (seq - last_seq) & 0xFFFFFFFF
                    if 1 < gap < 1000:
                        stats.rx_lost += gap - 1
                stats.rx += 1
            last_seq = seq

            samples = np.frombuffer(payload, dtype="<i2")
            with stats.lock:
                # widen first: np.abs(int16 -32768) overflows back to -32768
                stats.peak = int(np.max(np.abs(samples.astype(np.int32))))
            stream.write(samples)


def tx_loop(sock: socket.socket, pico_addr, input_device, stats: Stats):
    """Capture the local microphone and send it to the pico."""
    seq = 0
    with sd.InputStream(
        device=input_device,
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="int16",
        blocksize=BUFFER_SIZE,
        latency="low",
    ) as stream:
        while True:
            samples, _overflowed = stream.read(BUFFER_SIZE)
            sock.sendto(pack(DIR_TO_PICO, seq, samples.tobytes()), pico_addr)
            seq += 1
            with stats.lock:
                stats.tx += 1


def keepalive_loop(sock: socket.socket, pico_addr):
    """Header-only packets so the pico knows where to stream when we're not sending audio."""
    seq = 0
    while True:
        sock.sendto(pack(DIR_TO_PICO, seq), pico_addr)
        seq += 1
        time.sleep(KEEPALIVE_SECONDS)


def main():
    parser = argparse.ArgumentParser(description="Duplex UDP audio client for the pico")
    parser.add_argument("pico_ip", nargs="?", help="IP address of the pico")
    parser.add_argument("--port", type=int, default=1234)
    parser.add_argument("--local-port", type=int, default=0,
                        help="bind this local UDP port instead of an ephemeral one "
                             "(useful for tcpdump/firewall debugging)")
    parser.add_argument("--listen-only", action="store_true",
                        help="don't send microphone audio, only keep-alives")
    parser.add_argument("--input-device", default=None,
                        help="sounddevice input device (index or name)")
    parser.add_argument("--output-device", default=None,
                        help="sounddevice output device (index or name)")
    parser.add_argument("--list-devices", action="store_true")
    args = parser.parse_args()

    if args.list_devices:
        print(sd.query_devices())
        return

    if not args.pico_ip:
        parser.error("pico_ip is required (or use --list-devices)")

    # Tolerate the CIDR form the pico logs ("192.168.1.133/24").
    pico_ip = args.pico_ip.split("/", 1)[0]
    try:
        ipaddress.ip_address(pico_ip)
    except ValueError:
        parser.error(f"invalid pico IP: {args.pico_ip!r}")

    def parse_device(dev):
        if dev is None:
            return None
        return int(dev) if dev.isdigit() else dev

    pico_addr = (pico_ip, args.port)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", args.local_port))  # the pico replies to this port
    sock.settimeout(2.0)

    stats = Stats()

    def spawn(target, *target_args):
        """Run a worker thread; a dead worker means a dead client, so exit loudly."""

        def runner():
            try:
                target(*target_args)
            except Exception as e:
                print(f"fatal: {target.__name__}: {e}", file=sys.stderr)
                os._exit(1)

        threading.Thread(target=runner, daemon=True).start()

    spawn(rx_loop, sock, parse_device(args.output_device), stats)

    if args.listen_only:
        spawn(keepalive_loop, sock, pico_addr)
    else:
        spawn(tx_loop, sock, pico_addr, parse_device(args.input_device), stats)

    mode = "listen-only" if args.listen_only else "duplex"
    local_port = sock.getsockname()[1]
    print(
        f"{mode}: {BUFFER_SIZE} samples/block @ {SAMPLE_RATE} Hz "
        f"<-> {pico_addr[0]}:{pico_addr[1]} (local port {local_port})"
    )

    last_rx = 0
    last_time = time.time()
    try:
        while True:
            time.sleep(2.0)
            with stats.lock:
                now = time.time()
                rate = (stats.rx - last_rx) / (now - last_time)
                last_rx, last_time = stats.rx, now
                print(
                    f"rx={stats.rx} ({rate:.1f}pkt/s) lost={stats.rx_lost} "
                    f"bad={stats.rx_bad} tx={stats.tx} peak={stats.peak}"
                )
    except KeyboardInterrupt:
        print("\nbye")
        sys.exit(0)


if __name__ == "__main__":
    main()
