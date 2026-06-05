use crate::common::audio::AudioBlock;
use defmt::*;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_rp::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::Instant;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use embassy_usb::UsbDevice;
use static_cell::StaticCell;
static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; MAX_USB_BUF]> = StaticCell::new();
const MAX_USB_BUF: usize = 64;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});

// ---------- Helpers ----------
pub async fn write_cdc_chunked(
    cdc: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    data: &[u8],
) -> Result<(), embassy_usb::driver::EndpointError> {
    // CDC full-speed EPs are typically 64 bytes
    let max_packet = 64usize;
    let mut offset = 0usize;

    while offset < data.len() {
        let end = core::cmp::min(offset + max_packet, data.len());
        let chunk = &data[offset..end];

        // Ensure host is still connected
        cdc.wait_connection().await;

        // Write one packet
        cdc.write_packet(chunk).await?;
        offset = end;
    }
    Ok(())
}

pub fn init_usb(usb: Peri<'static, USB>) -> embassy_usb::Builder<'static, Driver<'static, USB>> {
    let driver = Driver::new(usb, Irqs);

    let usb_builder = embassy_usb::Builder::new(
        driver,
        {
            let mut cfg = embassy_usb::Config::new(0xc0de, 0xcafe);
            cfg.manufacturer = Some("Embassy");
            cfg.product = Some("Dual-Core ADC Stream");
            cfg.serial_number = Some("12345678");
            cfg.max_power = 100;
            cfg.max_packet_size_0 = MAX_USB_BUF as u8;
            cfg
        },
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; MAX_USB_BUF]),
    );

    usb_builder
}

// ---------- Core0: USB device run loop ----------
#[embassy_executor::task]
pub async fn usb_device_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) -> ! {
    info!("USB device task running");
    usb.run().await
}

// ---------- Core0: CDC TX task (owns the CDC class) ----------
#[embassy_executor::task]
pub async fn cdc_tx_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    cdc: &'static mut CdcAcmClass<'static, Driver<'static, USB>>,
) {
    info!("CDC TX task starting");

    let mut stats_timer = Instant::now();
    let mut blocks_ok = 0u32;
    let mut blocks_err = 0u32;

    loop {
        // Ensure host connected before we start draining
        cdc.wait_connection().await;

        // Drain audio blocks while connected
        loop {
            let mut block: AudioBlock = audio_channel.receive().await;
            block.centre_samples();
            let bytes: &[u8] = bytemuck::cast_slice(&block.samples);

            if let Err(e) = write_cdc_chunked(cdc, bytes).await {
                warn!("CDC write error: {:?}", e);
                blocks_err += 1;
                // Break to re-sync connection if it dropped / stalled
                break;
            } else {
                blocks_ok += 1;
            }

            if stats_timer.elapsed().as_secs() >= 2 {
                let total = blocks_ok + blocks_err;
                let pct = if total == 0 {
                    100.0
                } else {
                    (blocks_ok as f32 / total as f32) * 100.0
                };
                info!(
                    "USB Stats: {} ok, {} err ({}% ok)",
                    blocks_ok, blocks_err, pct
                );
                stats_timer = Instant::now();
            }
        }
    }
}
