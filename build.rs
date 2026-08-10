//! Copies the right `memory.x` for the target chip into a directory where the
//! linker can find it, and sets the linker args that chip needs.
//!
//! The RP2040 and RP2350 need different layouts and different link scripts:
//!   - RP2040 (thumbv6m) boots via a second-stage bootloader in a `BOOT2`
//!     region, and embassy-rp supplies `link-rp.x` to place it.
//!   - RP2350 (thumbv8m) boots via an `IMAGE_DEF` block that must land in the
//!     first 4K of flash. embassy-rp emits it into `.start_block`, which
//!     `memory-rp235x.x` positions, and it does *not* generate `link-rp.x` —
//!     passing `-Tlink-rp.x` on that target fails to link.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let is_rp2040 = target.starts_with("thumbv6m");

    let (memory_x, source) = if is_rp2040 {
        (
            &include_bytes!("memory-rp2040.x")[..],
            "memory-rp2040.x",
        )
    } else {
        (
            &include_bytes!("memory-rp235x.x")[..],
            "memory-rp235x.x",
        )
    };

    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(memory_x)
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    if is_rp2040 {
        println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    }
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
