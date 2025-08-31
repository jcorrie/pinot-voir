use defmt::*;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::{Instant, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use crate::common::adc_microphone::AudioBlock;
// ---------- Helpers ----------
pub async fn write_cdc_chunked(
    cdc: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    data: &[u8],
) -> Result<(), embassy_usb_driver::EndpointError> {
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
            let block: AudioBlock = audio_channel.receive().await;
            let centred_samples = block.centre_samples();
            let bytes: &[u8] = bytemuck::cast_slice(&centred_samples);

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
