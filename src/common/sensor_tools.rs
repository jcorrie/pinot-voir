use crate::common::shared_functions::{get_api_key_as_bearer_string, EnvironmentVariables};
use core::fmt::{Error, Write};
use defmt::info;
use embassy_dht::Reading;
use heapless::String;
use picoserve::response::IntoResponse;
use serde::{Deserialize, Serialize};
use picoserve::extract::Json;
use serde_json_core::to_string;
pub fn sensor_reading_to_string(reading: Reading<f32, f32>) -> Result<heapless::String<32>, Error> {
    let (temp, humi) = (reading.get_temp(), reading.get_hum());
    // Append static strings
    let mut body_string: heapless::String<32> = String::<32>::new();
    write!(body_string, "humidity={humi}&temperature={temp}")?;
    info!("Body string: {}", body_string);
    Ok(body_string)
}

pub fn ping_supabase_endpoint(environment_variables: &EnvironmentVariables) {
    let bearer_token = get_api_key_as_bearer_string(environment_variables.supabase_key)
        .expect("Failed to get API key as bearer string");
    let _headers = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("apikey", environment_variables.supabase_key),
        ("Authorization", bearer_token.as_str()),
    ];
    info!("Pinging Supabase endpoint");
}
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct SensorState {
    pub temperature: Option<f32>,
    pub humidity: Option<f32>,
    pub brightness: Option<f32>,
    pub loudness: Option<f32>,
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

// impl IntoResponse for SensorState {
//     async fn write_to<
//         R: picoserve::io::Read,
//         W: picoserve::response::ResponseWriter<Error = R::Error>,
//     >(
//         self,
//         connection: picoserve::response::Connection<'_, R>,
//         response_writer: W,
//     ) -> Result<picoserve::ResponseSent, W::Error> {
//         format_args!(
//             "{{\"temperature\":{:?},\"humidity\":{:?}}}",
//             self.temperature, self.humidity
//         )
//         .write_to(connection, response_writer)
//         .await
//     }
// }
