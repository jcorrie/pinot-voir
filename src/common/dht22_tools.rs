use crate::common::shared_functions::{get_api_key_as_bearer_string, EnvironmentVariables};
use core::fmt::{Error, Write};
use defmt::info;
use embassy_dht::Reading;
use heapless::String;
pub use embassy_dht::dht22::DHT22;

pub fn sensor_reading_to_string(reading: Reading<f32, f32>) -> Result<heapless::String<32>, Error> {
    let (temp, humi) = (reading.get_temp(), reading.get_hum());
    // Append static strings
    let mut body_string: heapless::String<32> = String::<32>::new();
    write!(body_string, "humidity={}&temperature={}", humi, temp)?;
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
