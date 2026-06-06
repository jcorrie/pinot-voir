import socket
import struct
import time
import numpy as np
import sounddevice as sd
from collections import deque

UDP_IP = "0.0.0.0"  # Listen on all interfaces
UDP_PORT = 1234
FRAME_RATE = 44100
CHANNELS = 1
SAMPLE_WIDTH = 2  # int16
BLOCK_SIZE = 512
MAX_PACKET_SIZE = 1440

def is_audio_data(data):
    if len(data) % 2 != 0:
        return False
    try:
        samples = struct.unpack(f"<{len(data) // 2}h", data)
        if all(-32768 <= sample <= 32767 for sample in samples):
            return True
    except struct.error:
        return False
    return True

print(f"Listening for UDP audio on port {UDP_PORT}...")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))
sock.setblocking(False)

# Use minimal buffering for low latency
audio_buffer = deque(maxlen=4)  
target_block_time = 512 / FRAME_RATE  # 11.6ms per block
last_play_time = time.time()

# Statistics
packets_received = 0
packets_played = 0
buffer_overruns = 0
buffer_underruns = 0

start_time = time.time()
last_stats_time = start_time

with sd.OutputStream(
    samplerate=FRAME_RATE,
    channels=CHANNELS,
    dtype="int16",
    blocksize=BLOCK_SIZE,
    latency="low",
) as stream:
    
    while True:
        current_time = time.time()
        
        # Receive packets (non-blocking)
        try:
            data, addr = sock.recvfrom(MAX_PACKET_SIZE)
            if data and len(data) == BLOCK_SIZE * 2:
                audio_array = np.frombuffer(data, dtype=np.int16)
                
                if len(audio_buffer) == audio_buffer.maxlen:
                    buffer_overruns += 1
                
                audio_buffer.append(audio_array)
                packets_received += 1
                
        except BlockingIOError:
            pass
        
        # Only play audio when it's time for the next block
        if current_time - last_play_time >= target_block_time:
            if len(audio_buffer) > 0:
                stream.write(audio_buffer.popleft())
                packets_played += 1
            else:
                silence = np.zeros(BLOCK_SIZE, dtype=np.int16)
                stream.write(silence)
                buffer_underruns += 1
            
            last_play_time = current_time
        
        # Print stats every 2 seconds
        current_time = time.time()
        if current_time - last_stats_time >= 2.0:
            elapsed = current_time - start_time
            print(f"Stats: {packets_received} received, {packets_played} played, "
                  f"{buffer_overruns} overruns, {buffer_underruns} underruns, "
                  f"buffer: {len(audio_buffer)}, rate: {packets_received/elapsed:.1f} pkt/s")
            last_stats_time = current_time
        
        # Small sleep to prevent busy waiting
        time.sleep(0.001)

sock.close()
print("Live UDP audio streaming finished.")