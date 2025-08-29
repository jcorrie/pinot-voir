import socket
import wave
import time

def udp_stream_to_file(listen_ip='0.0.0.0', listen_port=1234, output_file='output.raw', duration=5):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((listen_ip, listen_port))
    print(f"Listening for UDP packets on {listen_ip}:{listen_port}")

    end_time = time.time() + duration
    with open(output_file, 'wb') as f:
        while time.time() < end_time:
            sock.settimeout(max(0, end_time - time.time()))
            try:
                data, addr = sock.recvfrom(4096)
                print(f"Received {len(data)} bytes from {addr}")
                f.write(data)
                f.flush()
            except socket.timeout:
                break

def convert_raw_to_wav(raw_file, wav_file, channels=1, sampwidth=2, framerate=44100):
    with open(raw_file, 'rb') as rf:
        raw_data = rf.read()
    with wave.open(wav_file, 'wb') as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(sampwidth)
        wf.setframerate(framerate)
        wf.writeframes(raw_data)

if __name__ == "__main__":
    raw_file = 'output.raw'
    wav_file = 'output.wav'
    udp_stream_to_file(output_file=raw_file, duration=5)
    convert_raw_to_wav(raw_file, wav_file)
    print(f"Converted {raw_file} to {wav_file}")