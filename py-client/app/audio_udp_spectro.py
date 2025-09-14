import socket
import struct
import time
import numpy as np
import sounddevice as sd
from collections import deque
import matplotlib.pyplot as plt
from scipy import signal
from app.utils import FRAME_RATE

UDP_IP = "0.0.0.0"  # Listen on all interfaces
UDP_PORT = 1234
CHANNELS = 1
SAMPLE_WIDTH = 2  # int16
BLOCK_SIZE = 512
MAX_PACKET_SIZE = 1024
RECORD_DURATION = 10  # seconds to record before generating spectrogram

def _is_audio_data(data):
    if len(data) % 2 != 0:
        return False
    try:
        samples = struct.unpack(f"<{len(data) // 2}h", data)
        if all(-32768 <= sample <= 32767 for sample in samples):
            return True
    except struct.error:
        return False
    return True

def generate_spectrogram(audio_data):
    """Generate and save a spectrogram from audio data."""
    try:
        # Normalize audio data to float
        audio_float = audio_data.astype(np.float32) / 32768.0
        
        # Compute spectrogram using scipy
        f, t, Sxx = signal.spectrogram(audio_float, fs=FRAME_RATE, nperseg=1024)
        
        # Create the plot
        plt.figure(figsize=(12, 8))
        plt.pcolormesh(t, f, 10 * np.log10(Sxx + 1e-10), shading='gouraud')
        plt.ylabel('Frequency [Hz]')
        plt.xlabel('Time [sec]')
        plt.title('Audio Spectrogram')
        plt.colorbar(label='Power Spectral Density [dB]')
        
        # Save with timestamp
        timestamp = time.strftime("%Y%m%d_%H%M%S")
        filename = f"spectrogram_{timestamp}.png"
        plt.savefig(filename, dpi=150, bbox_inches='tight')
        plt.close()
        
        print(f"Spectrogram saved as {filename}")
        
    except Exception as e:
        print(f"Error generating spectrogram: {e}")
        # Fallback: simple numpy-based spectrogram
        try:
            # Simple FFT-based approach
            window_size = 1024
            hop_size = 512
            
            # Create spectrogram matrix
            n_windows = (len(audio_data) - window_size) // hop_size + 1
            n_freqs = window_size // 2 + 1
            spectrogram = np.zeros((n_freqs, n_windows))
            
            for i in range(n_windows):
                start = i * hop_size
                end = start + window_size
                if end > len(audio_data):
                    break
                    
                window = audio_data[start:end] * np.hanning(window_size)
                fft = np.fft.rfft(window)
                spectrogram[:, i] = np.abs(fft) ** 2
            
            # Simple plot without matplotlib
            print(f"Generated spectrogram matrix: {spectrogram.shape}")
            # Save as numpy array
            timestamp = time.strftime("%Y%m%d_%H%M%S")
            np.save(f"spectrogram_data_{timestamp}.npy", spectrogram)
            print(f"Spectrogram data saved as spectrogram_data_{timestamp}.npy")
            
        except Exception as e2:
            print(f"Fallback spectrogram generation failed: {e2}")

print(f"Listening for UDP audio on port {UDP_PORT}...")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))
sock.setblocking(False)

# Use minimal buffering for low latency
audio_buffer = deque(maxlen=2)  # Maximum 2 blocks
target_block_time = 512 / FRAME_RATE  # 11.6ms per block
last_play_time = time.time()

# Audio recording for spectrogram
recorded_audio = []
max_samples = int(FRAME_RATE * RECORD_DURATION)  # 10 seconds worth of samples

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
                
                # Store audio for spectrogram generation
                if len(recorded_audio) < max_samples:
                    recorded_audio.extend(audio_array)
                    if len(recorded_audio) >= max_samples:
                        print(f"Recorded {RECORD_DURATION} seconds of audio, generating spectrogram...")
                        generate_spectrogram(np.array(recorded_audio[:max_samples]))
                        recorded_audio.clear()  # Reset for next recording
                
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
        # time.sleep(0.001)

sock.close()
print("Live UDP audio streaming finished.")