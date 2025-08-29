import serial
import time
import wave

SERIAL_PORT = "/dev/ttyACM1"
BAUD_RATE = 115200
TIMEOUT = 1
RECORD_SECONDS = 5
RAW_FILENAME = "audio.raw"
WAV_FILENAME = "audio.wav"

# Audio parameters (adjust as needed)
CHANNELS = 1
SAMPLE_WIDTH = 2  # bytes (16-bit audio)
FRAME_RATE = 16000  # Hz

ser = serial.Serial(SERIAL_PORT, BAUD_RATE, timeout=TIMEOUT)

start_time = time.time()
with open(RAW_FILENAME, "wb") as f:
    while time.time() - start_time < RECORD_SECONDS:
        data = ser.read(2048)
        if data:
            f.write(data)

ser.close()

# Convert raw to wav
with open(RAW_FILENAME, "rb") as raw_file:
    raw_data = raw_file.read()

with wave.open(WAV_FILENAME, "wb") as wav_file:
    wav_file.setnchannels(CHANNELS)
    wav_file.setsampwidth(SAMPLE_WIDTH)
    wav_file.setframerate(FRAME_RATE)
    wav_file.writeframes(raw_data)
