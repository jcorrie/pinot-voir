#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Executor;
use embassy_rp::adc::InterruptHandler as ADCInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::USB;
use embassy_rp::time_driver::init;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use pinot_voir::common::adc_microphone::{AudioBlock, adc_task};
use pinot_voir::common::usb::{cdc_tx_task, init_usb, usb_device_task};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// ---------- Executors / Core stacks ----------
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// ---------- Audio channel between cores ----------
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

// ---------- USB/CDC statics ----------
const MAX_USB_BUF: usize = 64;
static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
static CDC_CLASS: StaticCell<CdcAcmClass<'static, Driver<'static, USB>>> = StaticCell::new();

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // ---------- Core1: ADC sampling ----------
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                spawner.spawn(defmt::unwrap!(adc_task(&AUDIO_CHANNEL, p.ADC, p.DMA_CH1, p.PIN_26)));
            });
        },
    );

    // ---------- Core0: USB + CDC ----------
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        let mut usb_builder = init_usb(p.USB);

        let cdc = CDC_CLASS.init(CdcAcmClass::new(
            &mut usb_builder,
            CDC_STATE.init(CdcState::new()),
            MAX_USB_BUF as u16, // max_packet_size for CDC EP
        ));

        let usb = usb_builder.build();

        // Run USB device + CDC TX task
        spawner.spawn(defmt::unwrap!(usb_device_task(usb)));
        spawner.spawn(defmt::unwrap!(cdc_tx_task(&AUDIO_CHANNEL, cdc)));
    });
}
