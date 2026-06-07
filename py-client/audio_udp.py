import socket
import time
import numpy as np
import sounddevice as sd
from collections import deque

UDP_IP = "0.0.0.0"
UDP_PORT = 1234
SAMPLE_RATE = 12000  # play at 12kHz so 720-sample blocks = 60ms each, matching ~16pkt/s WiFi rate
CHANNELS = 1
BUFFER_SIZE = 720   # must match i2s_microphone.rs BUFFER_SIZE
PACKET_BYTES = BUFFER_SIZE * 2  # 1440 bytes (int16)
MAX_PACKET_SIZE = 2048

print(f"Listening for UDP audio on port {UDP_PORT}...")
print(f"Expecting {BUFFER_SIZE} samples ({PACKET_BYTES} bytes) @ {SAMPLE_RATE}Hz")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))
sock.setblocking(False)

# Jitter buffer: grow to absorb bursts, shrink when empty
audio_buffer = deque()

packets_received = 0
packets_played = 0
packets_wrong_size = 0
buffer_overruns = 0
buffer_underruns = 0

start_time = time.time()
last_stats_time = start_time

# Use a callback-based output stream so playback timing is driven by the
# sound card, not our Python poll loop.
def audio_callback(outdata, frames, time_info, status):
    global packets_played, buffer_underruns
    if audio_buffer:
        block = audio_buffer.popleft()
        outdata[:, 0] = block
        packets_played += 1
    else:
        outdata[:] = 0
        buffer_underruns += 1

with sd.OutputStream(
    device=3,  # MacBook Pro Speakers
    samplerate=SAMPLE_RATE,
    channels=CHANNELS,
    dtype="int16",
    blocksize=BUFFER_SIZE,
    callback=audio_callback,
    latency="low",
):
    while True:
        # Drain all waiting UDP packets into the jitter buffer
        try:
            while True:
                data, _ = sock.recvfrom(MAX_PACKET_SIZE)
                if len(data) == PACKET_BYTES:
                    audio_buffer.append(np.frombuffer(data, dtype=np.int16).copy())
                    packets_received += 1
                    if len(audio_buffer) > 32:   # safety cap: ~2s of audio
                        audio_buffer.popleft()
                        buffer_overruns += 1
                else:
                    packets_wrong_size += 1
        except BlockingIOError:
            pass

        current_time = time.time()
        if current_time - last_stats_time >= 2.0:
            elapsed = current_time - start_time
            print(
                f"recv={packets_received} played={packets_played} "
                f"wrong={packets_wrong_size} over={buffer_overruns} "
                f"under={buffer_underruns} buf={len(audio_buffer)} "
                f"rate={packets_received/elapsed:.1f}pkt/s"
            )
            last_stats_time = current_time

        time.sleep(0.001)

sock.close()
