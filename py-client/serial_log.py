import serial
import time
import wave
import os
import struct

SERIAL_PORT = "/dev/ttyACM1"
BAUD_RATE = 115200
TIMEOUT = 0.1  # Shorter timeout for more responsive reading
RECORD_SECONDS = 5
RAW_FILENAME = "data/audio.raw"
WAV_FILENAME = "data/audio.wav"

# Audio parameters - must match your Pico settings
CHANNELS = 1
SAMPLE_WIDTH = 2  # bytes (16-bit audio)
FRAME_RATE = 2000  # Hz
EXPECTED_CHUNK_SIZE = 1024 * 2  # 1024 samples * 2 bytes per sample

# Calculate expected data rate for validation
EXPECTED_BYTES_PER_SECOND = FRAME_RATE * SAMPLE_WIDTH * CHANNELS

# Ensure the directory exists
os.makedirs(os.path.dirname(RAW_FILENAME), exist_ok=True)

def is_audio_data(data):
    """Simple heuristic to check if data looks like audio samples"""
    if len(data) % 2 != 0:
        return False  # Audio data should be even number of bytes
    
    # Check if data contains reasonable 16-bit values (not debug text)
    try:
        # Try to interpret as 16-bit samples
        samples = struct.unpack(f'<{len(data)//2}H', data)
        # ADC on Pico gives 12-bit values (0-4095), stored in 16-bit
        # Reasonable range check
        if all(0 <= sample <= 4095 for sample in samples):
            return True
    except struct.error:
        pass
    
    return False

print("Starting audio recording...")
ser = serial.Serial(SERIAL_PORT, BAUD_RATE, timeout=TIMEOUT)

# Clear any existing data in the buffer
ser.reset_input_buffer()
time.sleep(0.1)

start_time = time.time()
total_bytes = 0
valid_chunks = 0
invalid_chunks = 0

with open(RAW_FILENAME, "wb") as f:
    while time.time() - start_time < RECORD_SECONDS:
        # Read available data
        if ser.in_waiting > 0:
            data = ser.read(ser.in_waiting)
            
            if data:
                # Basic validation
                if is_audio_data(data):
                    f.write(data)
                    total_bytes += len(data)
                    valid_chunks += 1
                    
                    # Progress indicator
                    if valid_chunks % 100 == 0:
                        elapsed = time.time() - start_time
                        rate = total_bytes / elapsed if elapsed > 0 else 0
                        expected_rate = EXPECTED_BYTES_PER_SECOND
                        print(f"Progress: {elapsed:.1f}s, Rate: {rate:.0f} bytes/s "
                              f"(expected: {expected_rate} bytes/s)")
                else:
                    invalid_chunks += 1
                    # Print non-audio data (likely debug messages)
                    try:
                        debug_msg = data.decode('utf-8', errors='ignore').strip()
                        if debug_msg:
                            print(f"Debug: {debug_msg}")
                    except:
                        print(f"Invalid data chunk: {len(data)} bytes")
        
        # Small delay to prevent excessive CPU usage
        time.sleep(0.001)

ser.close()

# Statistics
elapsed_total = time.time() - start_time
actual_rate = total_bytes / elapsed_total if elapsed_total > 0 else 0
expected_samples = int(FRAME_RATE * elapsed_total)
actual_samples = total_bytes // SAMPLE_WIDTH

print(f"\nRecording Statistics:")
print(f"Duration: {elapsed_total:.2f} seconds")
print(f"Total bytes: {total_bytes}")
print(f"Valid chunks: {valid_chunks}")
print(f"Invalid chunks: {invalid_chunks}")
print(f"Data rate: {actual_rate:.0f} bytes/s (expected: {EXPECTED_BYTES_PER_SECOND})")
print(f"Samples: {actual_samples} (expected: {expected_samples})")
print(f"Sample rate accuracy: {(actual_samples/expected_samples)*100:.1f}%" if expected_samples > 0 else "N/A")

if total_bytes == 0:
    print("ERROR: No valid audio data received!")
    exit(1)

print("\nConverting to WAV...")

# Convert raw to wav
with open(RAW_FILENAME, "rb") as raw_file:
    raw_data = raw_file.read()

# Verify the data length makes sense
expected_file_size = int(FRAME_RATE * RECORD_SECONDS * SAMPLE_WIDTH * CHANNELS)
actual_file_size = len(raw_data)
print(f"File size: {actual_file_size} bytes (expected ~{expected_file_size} bytes)")

with wave.open(WAV_FILENAME, "wb") as wav_file:
    wav_file.setnchannels(CHANNELS)
    wav_file.setsampwidth(SAMPLE_WIDTH)
    wav_file.setframerate(FRAME_RATE)
    wav_file.writeframes(raw_data)

print(f"WAV file saved as {WAV_FILENAME}")
print(f"Duration of recorded audio: {len(raw_data) / (FRAME_RATE * SAMPLE_WIDTH * CHANNELS):.2f} seconds")