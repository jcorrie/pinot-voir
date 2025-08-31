use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_usb::class::cdc_acm::{CdcAcmClass};
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
