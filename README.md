# Pinot Voir

An experiment to build a device that can sense ambient temperature and humidity and post those values to a database. The original use case was to monitor temp/humidity in a room used to store wine.

Controller: Raspberry Pi Pico W. 

Sensor: DHT22

Dev environment: Windows/VS Code.

This repo also serves as a starting point for building other web-enabled projects on the Pico W.

### General 

* As you will see from [rust-toolchain.toml](/rust-toolchain.toml), we use a nightly build. Additionally, you will be building binaries for a non-native target, so run `rustup target add thumbv6m-none-eabi`.
* Holding down the bootsel button, connect your pico via USB to your machine. The pico should be registered as a new storage device on your machine (similar to plugging in a USB thumb drive).
* You can build a binary using `cargo build` or `cargo build --release` in the usual way.
* To flash your binary to the pico (assuming no debug probe) use `cargo run --bin [binary-name] --release`. Cargo will follow instructions in [config.toml](.cargo/config.toml) and transfer the binary onto your pico using the selected runner.
* To re-flash, you will need to disconnect the pico and reconnect it holding down the bootsel button.

### Debugging

The [debug probe](https://thepihut.com/products/raspberry-pi-debug-probe) will be very helpful for logging statements to the terminal. Use `cargo install probe-rs-tools --locked` to install the tooling.

In [config.toml](.cargo/config.toml) toggle (comment in/out) the relevant runner.

To flash, just run `cargo run --bin [binary-name]` (no `--release` flag). You don't need to disconnect/connect each time you want to reflash.

You may need to install `sudo apt install openocd`.

### DHT22 wiring

Courtesy of [adafruit](https://learn.adafruit.com/dht-humidity-sensing-on-raspberry-pi-with-gdocs-logging/wiring), here is an image of how the DHT22 would be wired up. These pins don't correspond exactly to the pico, so just make sure that the pico's ground pin is connected to the ground rail on your breadboard and the 3V pin is connected to the positive rail on your breadboard. Additionally, this repo assumes that the data line out of the DHT22 sensor (blue in the diagram below) is connected to GPIO PIN 16.

![Alt text](https://cdn-learn.adafruit.com/assets/assets/000/001/861/large1024/raspberry_pi_dht22wiring.gif?1447864317 "DHT22 wiring guide")

### Server

The wonderful [picoserve](https://github.com/sammhicks/picoserve) crate is used to turn the pico into a server. 

It uses the 2.4 ghz band and once connected you should be able to ping it (using WSL, if on Windows): `ping 192.168.1.85`. Or visit [192.168.1.85](http://192.168.1.85) in your browser.

Create a .env file from .env.example, with details of the wifi network the pico will connect to.

### Other learnings (for dummies) from embedded rust development

* The std library is not implemented for the pico environment. 
* There is no default memory allocator for the pico environment. Accordingly, you do not have access to types like String (which are allocated on the heap at runtime). The heapless library can provide some workarounds here.
* Normally trivial tasks such as formatting strings become complicated in an embedded environment.
* A work in progress is breaking down the code into more reusable functions. The borrow checker has been interfering (eg, passing peripherals into functions is causing issues).
* Resource constraints are real. Running both a server and making periodic `GET` requests seems to have pushed the device's memory to its limits.
