#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]

use defmt::*;
use embassy_executor::Executor;
use embassy_rp::adc::InterruptHandler as ADCInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler as USBInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use pinot_voir::common::adc_microphone::{AudioBlock, adc_task};
use pinot_voir::common::usb::{cdc_tx_task, usb_device_task};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// ---------- Interrupts ----------
bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => USBInterruptHandler<USB>;
});
bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// ---------- Executors / Core stacks ----------
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

// ---------- Audio channel between cores ----------
static AUDIO_CHANNEL: SyncChannel<CriticalSectionRawMutex, AudioBlock, 4> = SyncChannel::new();

// ---------- USB/CDC statics ----------
static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; MAX_USB_BUF]> = StaticCell::new();
static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
static CDC_CLASS: StaticCell<CdcAcmClass<'static, Driver<'static, USB>>> = StaticCell::new();
const MAX_USB_BUF: usize = 64;

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
                unwrap!(spawner.spawn(adc_task(&AUDIO_CHANNEL, p.ADC, p.DMA_CH0, p.PIN_26)));
            });
        },
    );

    // ---------- Core0: USB + CDC ----------
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        let driver = Driver::new(p.USB, Irqs);

        let mut usb_builder = embassy_usb::Builder::new(
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

        let cdc = CDC_CLASS.init(CdcAcmClass::new(
            &mut usb_builder,
            CDC_STATE.init(CdcState::new()),
            MAX_USB_BUF as u16, // max_packet_size for CDC EP
        ));

        let usb = usb_builder.build();

        // Run USB device + CDC TX task
        unwrap!(spawner.spawn(usb_device_task(usb)));
        unwrap!(spawner.spawn(cdc_tx_task(&AUDIO_CHANNEL, cdc)));
    });
}
