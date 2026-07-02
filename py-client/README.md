# py-client

Desktop-side tools for the pico audio experiments.

## audio_udp.py — duplex UDP audio client

Counterpart to the `audio_duplex` (and mic-only `i2s_wifi`) pico binaries.
Sends your microphone to the pico and plays the pico's microphone stream.

The client always initiates: the pico binds UDP port 1234 and streams back
to whoever talks to it, so start the pico first, then run:

```sh
uv run audio_udp.py <pico-ip>                 # full duplex
uv run audio_udp.py <pico-ip> --listen-only   # receive only
uv run audio_udp.py --list-devices            # pick input/output devices
uv run audio_udp.py <pico-ip> --input-device 1 --output-device 3
```

The pico's IP is logged over defmt/probe-rs when it joins the network
("Current IPv4 configuration: ...").

Audio is 48 kHz mono 16-bit in 720-sample blocks (15 ms, ~66.7 packets/s,
~0.96 Mbit/s each way). The wire format is documented in
`src/common/audio.rs`; both directions share port 1234 and are told apart
by a direction byte in the 12-byte header, with sequence numbers for loss
tracking (printed as `lost=` in the stats line).
