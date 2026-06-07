import socket
import struct
import time
import numpy as np
import sounddevice as sd
from collections import deque

UDP_IP = "0.0.0.0"  # Listen on all interfaces
UDP_PORT = 1234
SAMPLE_RATE = 48000
CHANNELS = 1
SAMPLE_WIDTH = 2  # int16
BUFFER_SIZE = 720  # samples per packet — must match i2s_microphone.rs BUFFER_SIZE
PACKET_BYTES = BUFFER_SIZE * SAMPLE_WIDTH  # 1440 bytes
BLOCK_DURATION = BUFFER_SIZE / SAMPLE_RATE  # 15ms per block
MAX_PACKET_SIZE = 2048  # generous receive buffer

print(f"Listening for UDP audio on port {UDP_PORT}...")
print(f"Expecting {BUFFER_SIZE} samples ({PACKET_BYTES} bytes) per packet @ {SAMPLE_RATE}Hz")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))
sock.setblocking(False)

# Jitter buffer: hold a few blocks to smooth over timing variation
audio_buffer = deque(maxlen=8)
last_play_time = time.time()

# Statistics
packets_received = 0
packets_played = 0
packets_wrong_size = 0
buffer_overruns = 0
buffer_underruns = 0

start_time = time.time()
last_stats_time = start_time

with sd.OutputStream(
    samplerate=SAMPLE_RATE,
    channels=CHANNELS,
    dtype="int16",
    blocksize=BUFFER_SIZE,
    latency="low",
) as stream:

    while True:
        current_time = time.time()

        # Drain all available packets into the jitter buffer
        try:
            while True:
                data, addr = sock.recvfrom(MAX_PACKET_SIZE)
                if len(data) == PACKET_BYTES:
                    audio_array = np.frombuffer(data, dtype=np.int16).copy()

                    if len(audio_buffer) == audio_buffer.maxlen:
                        buffer_overruns += 1

                    audio_buffer.append(audio_array)
                    packets_received += 1
                else:
                    packets_wrong_size += 1
        except BlockingIOError:
            pass

        # Play one block per block-duration tick
        if current_time - last_play_time >= BLOCK_DURATION:
            if len(audio_buffer) > 0:
                stream.write(audio_buffer.popleft())
                packets_played += 1
            else:
                silence = np.zeros(BUFFER_SIZE, dtype=np.int16)
                stream.write(silence)
                buffer_underruns += 1

            last_play_time = current_time

        # Print stats every 2 seconds
        current_time = time.time()
        if current_time - last_stats_time >= 2.0:
            elapsed = current_time - start_time
            print(
                f"Stats: {packets_received} received, {packets_played} played, "
                f"{packets_wrong_size} wrong-size, "
                f"{buffer_overruns} overruns, {buffer_underruns} underruns, "
                f"buffer: {len(audio_buffer)}, rate: {packets_received/elapsed:.1f} pkt/s"
            )
            last_stats_time = current_time

        # Small sleep to prevent busy waiting
        time.sleep(0.001)

sock.close()
print("Live UDP audio streaming finished.")
