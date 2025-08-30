import serial
import time
import wave
import os
import struct

SERIAL_PORT = "/dev/ttyACM1"
BAUD_RATE = 115200
TIMEOUT = 0.1
RECORD_SECONDS = 5
RAW_FILENAME = "data/audio.raw"
WAV_FILENAME = "data/audio.wav"

# Audio parameters - must match your Pico settings
CHANNELS = 1
SAMPLE_WIDTH = 2  # bytes (16-bit audio)
FRAME_RATE = 44100  # Hz
EXPECTED_CHUNK_SIZE = 1024 * 2  # 1024 samples * 2 bytes per sample

# Calculate expected data rate for validation
EXPECTED_BYTES_PER_SECOND = FRAME_RATE * SAMPLE_WIDTH * CHANNELS

# Ensure the directory exists
os.makedirs(os.path.dirname(RAW_FILENAME), exist_ok=True)


def is_audio_data(data):
    """Check if data looks like centered audio samples from Pico"""
    if len(data) % 2 != 0:
        return False  # Audio data should be even number of bytes

    # Check if data contains reasonable signed 16-bit values
    try:
        # Interpret as signed 16-bit samples (i16 from Rust)
        samples = struct.unpack(
            f"<{len(data) // 2}h", data
        )  # Changed to 'h' for signed

        # After centering (raw - 2048), range should be roughly -2048 to +2047
        # Allow some tolerance for noise/variations
        reasonable_samples = 0
        for sample in samples:
            if -2200 <= sample <= 2200:  # Slightly wider range for tolerance
                reasonable_samples += 1

        # Consider it audio data if most samples are in reasonable range
        if reasonable_samples >= len(samples) * 0.8:  # 80% threshold
            return True

    except struct.error:
        pass

    return False


def convert_to_unsigned(signed_data):
    """Convert signed 16-bit samples back to unsigned for WAV file"""
    # Unpack as signed 16-bit
    signed_samples = struct.unpack(f"<{len(signed_data) // 2}h", signed_data)

    # Convert back to unsigned 16-bit by adding 2048 and ensuring range
    unsigned_samples = []
    for sample in signed_samples:
        # Add back the DC offset and clamp to valid range
        unsigned = max(0, min(4095, sample + 2048))
        unsigned_samples.append(unsigned)

    # Pack back as unsigned 16-bit
    return struct.pack(f"<{len(unsigned_samples)}H", *unsigned_samples)


print("Starting audio recording...")
ser = serial.Serial(SERIAL_PORT, BAUD_RATE, timeout=TIMEOUT)

# Clear any existing data in the buffer
ser.reset_input_buffer()
time.sleep(0.1)

start_time = time.time()
total_bytes = 0
valid_chunks = 0
invalid_chunks = 0
raw_signed_data = bytearray()

with open(RAW_FILENAME + ".signed", "wb") as f_signed:
    while time.time() - start_time < RECORD_SECONDS:
        # Read available data
        if ser.in_waiting > 0:
            data = ser.read(ser.in_waiting)

            if data:
                # Basic validation
                if is_audio_data(data):
                    f_signed.write(data)  # Save signed data
                    raw_signed_data.extend(data)
                    total_bytes += len(data)
                    valid_chunks += 1

                    # Progress indicator
                    if valid_chunks % 100 == 0:
                        elapsed = time.time() - start_time
                        rate = total_bytes / elapsed if elapsed > 0 else 0
                        expected_rate = EXPECTED_BYTES_PER_SECOND
                        print(
                            f"Progress: {elapsed:.1f}s, Rate: {rate:.0f} bytes/s "
                            f"(expected: {expected_rate} bytes/s)"
                        )

                        # Show sample values for debugging
                        if len(data) >= 6:
                            samples = struct.unpack("<3h", data[:6])
                            print(f"Sample values: {samples}")
                else:
                    invalid_chunks += 1
                    # Print non-audio data (likely debug messages)
                    try:
                        debug_msg = data.decode("utf-8", errors="ignore").strip()
                        if debug_msg:
                            print(f"Debug: {debug_msg}")
                    except:
                        print(f"Invalid data chunk: {len(data)} bytes")
                        # Show first few bytes for debugging
                        if len(data) >= 6:
                            print(f"First 6 bytes: {list(data[:6])}")

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
print(
    f"Sample rate accuracy: {(actual_samples / expected_samples) * 100:.1f}%"
    if expected_samples > 0
    else "N/A"
)

if total_bytes == 0:
    print("ERROR: No valid audio data received!")
    exit(1)

# Show sample statistics
if raw_signed_data:
    signed_samples = struct.unpack(f"<{len(raw_signed_data) // 2}h", raw_signed_data)
    min_sample = min(signed_samples)
    max_sample = max(signed_samples)
    avg_sample = sum(signed_samples) / len(signed_samples)
    print(f"Sample range: {min_sample} to {max_sample} (avg: {avg_sample:.1f})")

print("\nConverting to WAV...")

# Convert signed samples back to unsigned for WAV format
unsigned_data = convert_to_unsigned(raw_signed_data)

# Save the unsigned data as raw file
with open(RAW_FILENAME, "wb") as raw_file:
    raw_file.write(unsigned_data)

# Verify the data length makes sense
expected_file_size = int(FRAME_RATE * RECORD_SECONDS * SAMPLE_WIDTH * CHANNELS)
actual_file_size = len(unsigned_data)
print(f"File size: {actual_file_size} bytes (expected ~{expected_file_size} bytes)")

# Create WAV file
with wave.open(WAV_FILENAME, "wb") as wav_file:
    wav_file.setnchannels(CHANNELS)
    wav_file.setsampwidth(SAMPLE_WIDTH)
    wav_file.setframerate(FRAME_RATE)
    wav_file.writeframes(unsigned_data)

print(f"WAV file saved as {WAV_FILENAME}")
print(
    f"Duration of recorded audio: {len(unsigned_data) / (FRAME_RATE * SAMPLE_WIDTH * CHANNELS):.2f} seconds"
)
