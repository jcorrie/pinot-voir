import serial

ser = serial.Serial(
    "/dev/ttyACM1", 115200, timeout=1
)  # Adjust baud rate and port as needed

with open("audio.raw", "wb") as f:
    while True:
        data = ser.read(2048)  # Read bytes
        if data:
            f.write(data)
