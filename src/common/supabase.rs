use core::str::from_utf8;

use super::dht22_tools::sensor_reading_to_string;
use super::shared_functions::EnvironmentVariables;
use defmt::{error, info, unwrap};
use embassy_net::tcp::client::TcpConnection;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use reqwless::client::{HttpClient, HttpConnection, TlsConfig, TlsVerify};
use reqwless::response::Response;

use {defmt_rtt as _, panic_probe as _};

pub fn construct_post_request_arguments(
    dht_reading: embassy_dht::Reading<f32, f32>,
    environment_variables: &EnvironmentVariables,
) -> Result<(heapless::String<32>, [(&str, &str); 3]), core::fmt::Error> {
    let dht_reading_as_string: heapless::String<32> = match sensor_reading_to_string(dht_reading) {
        Ok(s) => s,
        Err(_e) => {
            error!("Failed to convert sensor reading to string");
            return Err(_e);
        }
    };
    info!("DHT reading as string: {:?}", &dht_reading_as_string);

    let headers: [(&str, &str); 3] = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("apikey", environment_variables.supabase_key),
        ("Authorization", environment_variables.supabase_key),
    ];
    Ok((dht_reading_as_string, headers))
}

pub async fn read_http_response(
    response: Response<'_, '_, HttpConnection<'_, TcpConnection<'_, 1, 1024, 1024>>>,
) {
    let body = match from_utf8(response.body().read_to_end().await.unwrap()) {
        Ok(b) => b,
        Err(_e) => {
            error!("Failed to read response body");
            return; // handle the error
        }
    };
    info!("Response body: {:?}", &body);
}
