# Pinot Voir

An experiment to build a device that can sense ambient temperature and humidity and post those values to a database. The original use case was to monitor temp/humidity in a room used to store wine.

Controller: Raspberry Pi Pico W. 

Sensor: DHT22

Dev environment: Windows/VS Code.

This repo also serves as a starting point for building other web-enabled projects on the Pico W.

### General 

* As you will see from [rust-toolchain.toml](/rust-toolchain.toml), we use a nightly build. Additionally, you will be building binaries for a non-native target, so run `rustup target add thumbv6m-none-eabi`.
* Holding down the bootsel button, connect your pico via USB to your machine. The pico should be registered as a new storage device on your machine (similar to plugging in a USB thumb drive).
* Install `cargo install elf2uf2-rs`
* You can build a binary using `cargo build` or `cargo build --release` in the usual way.
* To flash your binary to the pico (assuming no debug probe) use `cargo run --bin [binary-name] --release`. Cargo will follow instructions in [config.toml](.cargo/config.toml) and transfer the binary onto your pico using the selected runner.
* To re-flash, you will need to disconnect the pico and reconnect it holding down the bootsel button.

### Debugging

The [debug probe](https://thepihut.com/products/raspberry-pi-debug-probe) will be very helpful for logging statements to the terminal. Use `cargo install probe-rs-tools --locked` to install the tooling.

In [config.toml](.cargo/config.toml) toggle (comment in/out) the relevant runner.

To flash, just run `cargo run --bin [binary-name]` (no `--release` flag). You don't need to disconnect/connect each time you want to reflash.

You may need to install `sudo apt install -y pkg-config libudev-dev cmake git`.

### DHT22 wiring

Courtesy of [adafruit](https://learn.adafruit.com/dht-humidity-sensing-on-raspberry-pi-with-gdocs-logging/wiring), here is an image of how the DHT22 would be wired up. These pins don't correspond exactly to the pico, so just make sure that the pico's ground pin is connected to the ground rail on your breadboard and the 3V pin is connected to the positive rail on your breadboard. Additionally, this repo assumes that the data line out of the DHT22 sensor (blue in the diagram below) is connected to GPIO PIN 16.

![Alt text](https://cdn-learn.adafruit.com/assets/assets/000/001/861/large1024/raspberry_pi_dht22wiring.gif?1447864317 "DHT22 wiring guide")

### Server

The wonderful [picoserve](https://github.com/sammhicks/picoserve) crate is used to turn the pico into a server. 

It uses the 2.4 ghz band and once connected you should be able to ping it (using WSL, if on Windows): `ping 192.168.1.85`. Or visit [192.168.1.85](http://192.168.1.85) in your browser.

Create a .env file from .env.example, with details of the wifi network the pico will connect to.

### Intercom (`audio_intercom`)

A push-to-talk client for the audio room in [SPQR](https://github.com/jcorrie/spqr).
The wire format is documented in that repo at `docs/audio-protocol.md`: raw PCM16,
mono, 16 kHz, 20 ms frames, one datagram per frame, no header. Sending is joining —
the room mix comes back to whatever port the datagrams were sent from, so one
socket serves both directions.

Set `AUDIO_SERVER_IP` in `.env` to the LAN address of the server, then
`cargo run --bin audio_intercom --release`.

**Half duplex.** Hold the button to talk, release to listen. The two are never
live at once: while the button is down, incoming room audio is discarded rather
than played. There is no acoustic echo canceller — an M0+ is not going to run one —
so if the microphone and speaker share an enclosure, this is what stops your
speaker feeding your microphone and sending everyone else their own voice back.
The onboard LED is lit while you are transmitting.

Wiring:

| Signal | GPIO | Physical pin |
|---|---|---|
| Mic BCLK | 18 | 24 |
| Mic LRCLK | 19 | 25 |
| Mic DOUT | 20 | 26 |
| DAC DIN | 9 | 12 |
| DAC BCLK | 10 | 14 |
| DAC LRCLK | 11 | 15 |
| PTT button | 15 | 20 |

The button just shorts GPIO 15 to ground — the internal pull-up does the rest, so
no external parts. Each pair of clocks has to be consecutive GPIOs in that order,
because PIO drives them as one contiguous side-set range. Tie the microphone's
channel select high (right channel).

Notes:

* The I2S bus runs at 48 kHz, three times the wire rate, because the common MEMS
  microphones (SPH0645LM4H, INMP441) are only specified down to 32 kHz. A 27-tap
  Q15 FIR in `common/resample.rs` handles both directions; it costs about 6% of a
  core and has 0.5 ms of group delay.
* `I2S_BIT_DEPTH` puts BCLK at 1.536 MHz. Some class-D amps want a faster bit
  clock than that — see the comment on the constant if yours will not lock.
* Compute is not the constraint here. Sustained load is roughly 512 kbit/s of UDP
  and a few percent of one core; the build uses ~70 KB of the RP2040's 264 KB of
  RAM. What does matter is that the capture clock, the playback clock and the
  server's 50 Hz are three free-running oscillators, which is what the jitter
  buffer in `common/intercom.rs` absorbs.

### Other learnings (for dummies) from embedded rust development

* The std library is not implemented for the pico environment. 
* There is no default memory allocator for the pico environment. Accordingly, you do not have access to types like String (which are allocated on the heap at runtime). The heapless library can provide some workarounds here.
* Normally trivial tasks such as formatting strings become complicated in an embedded environment.
* A work in progress is breaking down the code into more reusable functions. The borrow checker has been interfering (eg, passing peripherals into functions is causing issues).
* Resource constraints are real. Running both a server and making periodic `GET` requests seems to have pushed the device's memory to its limits.
