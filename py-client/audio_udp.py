import socket
import time
import numpy as np
import sounddevice as sd

UDP_PORT = 1234
SAMPLE_RATE = 48000
BUFFER_SIZE = 720   # must match i2s_microphone.rs BUFFER_SIZE
PACKET_BYTES = BUFFER_SIZE * 2  # 1440 bytes (int16)
MAX_PACKET_SIZE = 2048

print(f"Listening for UDP audio on port {UDP_PORT}...")
print(f"Expecting {BUFFER_SIZE} samples ({PACKET_BYTES} bytes) @ {SAMPLE_RATE}Hz")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("0.0.0.0", UDP_PORT))
sock.settimeout(2.0)

packets_received = 0
packets_wrong_size = 0
start_time = time.time()
last_stats_time = start_time

with sd.OutputStream(
    device=3,  # MacBook Pro Speakers
    samplerate=SAMPLE_RATE,
    channels=1,
    dtype="int16",
    blocksize=BUFFER_SIZE,
    latency="low",
) as stream:
    while True:
        try:
            data, _ = sock.recvfrom(MAX_PACKET_SIZE)
        except socket.timeout:
            print("(no packet for 2s)")
            continue

        if len(data) != PACKET_BYTES:
            packets_wrong_size += 1
            continue

        samples = np.frombuffer(data, dtype=np.int16)
        peak = int(np.max(np.abs(samples)))
        stream.write(samples)
        packets_received += 1

        current_time = time.time()
        if current_time - last_stats_time >= 2.0:
            elapsed = current_time - start_time
            print(
                f"recv={packets_received} wrong={packets_wrong_size} "
                f"rate={packets_received/elapsed:.1f}pkt/s  peak={peak}"
            )
            last_stats_time = current_time
