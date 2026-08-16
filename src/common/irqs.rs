//! The crate's single set of interrupt bindings.
//!
//! `bind_interrupts!` emits the interrupt symbol itself, so a given interrupt may
//! only be bound once per binary. Because the library binds PIO, ADC, USB and DMA
//! interrupts, any binary that links this crate must reuse these bindings rather
//! than declaring its own — a second binding of the same interrupt is a duplicate
//! symbol at link time.
//!
//! embassy-rp 0.10 additionally requires a bound `DMA_IRQ_0` handler per channel
//! that gets turned into a `dma::Channel`; DMA_CH0 (wifi), DMA_CH1 (ADC
//! microphone / I2S in) and DMA_CH2 (I2S out) are all listed below.
//!
//! PIO0 belongs to the CYW43 SPI link. PIO1 carries the I2S state machines used
//! by the intercom — SM0 in from the microphone, SM1 out to the DAC.

use embassy_rp::adc::InterruptHandler as AdcInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, PIO0, PIO1, USB};
use embassy_rp::pio::InterruptHandler as PioInterruptHandler;
use embassy_rp::usb::InterruptHandler as UsbInterruptHandler;

bind_interrupts!(pub struct Irqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    PIO1_IRQ_0 => PioInterruptHandler<PIO1>;
    ADC_IRQ_FIFO => AdcInterruptHandler;
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>, dma::InterruptHandler<DMA_CH2>;
});
