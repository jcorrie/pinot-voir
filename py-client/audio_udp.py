import socket
import struct
import time
import numpy as np
import sounddevice as sd

UDP_IP = "0.0.0.0"  # Listen on all interfaces
UDP_PORT = 1234
FRAME_RATE = 44100
CHANNELS = 1
SAMPLE_WIDTH = 2  # int16
RECORD_SECONDS = 515
MAX_PACKET_SIZE = 1024  # should match what your Pico sends


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

start_time = time.time()

with sd.OutputStream(
    samplerate=FRAME_RATE, channels=CHANNELS, dtype="int16", blocksize=0
) as stream:
    while time.time() - start_time < RECORD_SECONDS:
        try:
            data, addr = sock.recvfrom(MAX_PACKET_SIZE)
            if data and is_audio_data(data):
                audio_array = np.frombuffer(data, dtype=np.int16)
                stream.write(audio_array)
        except BlockingIOError:
            # No data available yet
            time.sleep(0.001)

sock.close()
print("Live UDP audio streaming finished.")
