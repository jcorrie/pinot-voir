# py-client

Desktop-side tools for the pico audio experiments.

## `audio_room.py` — stand-in for the SPQR audio room

Counterpart to the `audio_intercom` pico binary. Binds the room's UDP port,
plays whatever the pico sends, and sends your microphone back, so the firmware
can be brought up without running the SPQR server.

```sh
uv run audio_room.py                        # duplex
uv run audio_room.py --listen-only          # play the pico, send nothing
uv run audio_room.py --list-devices         # pick input/output devices
uv run audio_room.py --input-device 1 --output-device 3
```

The pico initiates: it sends to `AUDIO_SERVER_IP` from `.env`, so point that at
this machine and start either side in any order. Hold PTT and the `rx` counter
should climb at 50/s with a `peak` that tracks your voice.

Reading the stats line when it doesn't:

| Symptom | Meaning |
|---|---|
| `rx=0`, `keepalive` climbing | pico is registered but sending no audio — PTT not reaching GPIO 22 |
| `rx` climbing, `peak=0` | mic path dead; check the `i2s raw` line in the pico's defmt log |
| `bad` climbing | frame size mismatch — the two sides disagree about the protocol |
| `dropped` climbing | this machine is not draining playback fast enough |

`--squelch` sets the level below which a frame counts as silence and is not
sent, mirroring the server, which never transmits silence. That gap is a path
the firmware has to survive, so leaving it on is the more honest test; pass
`--squelch 0` to stream continuously.

## `serial_log.py`, `stream_audio.py`, `main.py`

Older one-off scripts from the USB/serial audio experiments. `stream_audio.py`
plays a 44.1 kHz stream off the serial port; `main.py` dumps raw UDP to a file.
