import serial
import time
import struct
import sounddevice as sd
from app.utils import FRAME_RATE

SERIAL_PORT = "/dev/ttyACM1"
BAUD_RATE = 115200
TIMEOUT = 0.1
CHANNELS = 1
SAMPLE_WIDTH = 2
RECORD_SECONDS = 515

def is_audio_data(data):
    if len(data) % 2 != 0:
        return False
    try:
        samples = struct.unpack(f"<{len(data) // 2}h", data)
        if all(-32768 <= sample <= 32767 for sample in samples):
            return True
    except struct.error:
        pass
    return False

print("Starting live audio stream...")
ser = serial.Serial(SERIAL_PORT, BAUD_RATE, timeout=TIMEOUT)
ser.reset_input_buffer()
time.sleep(0.1)

start_time = time.time()

with sd.OutputStream(
    samplerate=FRAME_RATE,
    channels=CHANNELS,
    dtype='int16',
    blocksize=0
) as stream:
    while time.time() - start_time < RECORD_SECONDS:
        if ser.in_waiting > 0:
            data = ser.read(ser.in_waiting)
            if data and is_audio_data(data):
                # Convert bytes to numpy array for playback
                import numpy as np
                audio_array = np.frombuffer(data, dtype=np.int16)
                stream.write(audio_array)
        time.sleep(0.001)

ser.close()
print("Live audio streaming finished.")
