use core::fmt::{Error, Write};
use defmt::info;
use embassy_dht::Reading;
use heapless::String;
use serde::{Deserialize, Serialize};
pub fn sensor_reading_to_string(reading: Reading<f32, f32>) -> Result<heapless::String<32>, Error> {
    let (temp, humi) = (reading.get_temp(), reading.get_hum());
    // Append static strings
    let mut body_string: heapless::String<32> = String::<32>::new();
    write!(body_string, "humidity={humi}&temperature={temp}")?;
    info!("Body string: {}", body_string);
    Ok(body_string)
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct SensorState {
    pub temperature: Option<f32>,
    pub humidity: Option<f32>,
    pub brightness: Option<f32>,
    pub loudness: Option<f32>,
}

impl Default for SensorState {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorState {
    pub fn new() -> SensorState {
        SensorState {
            temperature: None,
            humidity: None,
            brightness: None,
            loudness: None,
        }
    }
}
