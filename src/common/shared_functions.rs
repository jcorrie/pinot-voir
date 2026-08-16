use core::fmt::{Error, Write};
use core::module_path;
use defmt::info;
use embassy_time::{Duration, Timer};
use heapless::String;

use {defmt_rtt as _, panic_probe as _};

pub async fn blink_n_times(control: &mut cyw43::Control<'_>, n: i32) {
    for _ in 0..n {
        info!("led on!");
        control.gpio_set(0, true).await;
        Timer::after(Duration::from_millis(300)).await;

        info!("led off!");
        control.gpio_set(0, false).await;
        Timer::after(Duration::from_millis(300)).await;
    }
}

#[derive(Debug, Copy, Clone)]
pub struct EnvironmentVariables {
    pub wifi_ssid: &'static str,
    pub wifi_password: &'static str,
    pub supabase_url: &'static str,
    pub supabase_key: &'static str,
    pub server_url: &'static str,
    /// LAN address of the SPQR audio room, dotted quad. Optional so that the
    /// binaries that have nothing to do with audio still build from a `.env`
    /// that predates it; `audio_intercom` is the one that requires it.
    pub audio_server_ip: Option<&'static str>,
}

impl Default for EnvironmentVariables {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvironmentVariables {
    pub fn new() -> EnvironmentVariables {
        let env_file: &str = include_str!("../../.env");
        let mut ssid: Option<&str> = None;
        let mut password: Option<&str> = None;
        let mut supabase_url: Option<&str> = None;
        let mut supabase_key: Option<&str> = None;
        let mut server_url: Option<&str> = None;
        let mut audio_server_ip: Option<&str> = None;

        for line in env_file.lines() {
            if let Some((key, value)) = line.split_once('=') {
                match key {
                    "WIFI_SSID" => ssid = Some(value),
                    "PASSWORD" => password = Some(value),
                    "SUPABASE_URL" => supabase_url = Some(value),
                    "SUPABASE_KEY" => supabase_key = Some(value),
                    "SERVER_URL" => server_url = Some(value),
                    "AUDIO_SERVER_IP" => audio_server_ip = Some(value),
                    _ => {}
                }
            }
        }

        let wifi_ssid = ssid.expect("SSID not found in .env file");
        let wifi_password = password.expect("Password not found in .env file");
        let supabase_url = supabase_url.expect("Supabase URL not found in .env file");
        let supabase_key = supabase_key.expect("Supabase key not found in .env file");
        let server_url = server_url.expect("Server url not found in .env file");

        EnvironmentVariables {
            wifi_ssid,
            wifi_password,
            supabase_url,
            supabase_key,
            server_url,
            audio_server_ip,
        }
    }
}

pub fn get_api_key_as_bearer_string(api_key: &str) -> Result<heapless::String<256>, Error> {
    // Append static strings
    let mut owned_string: heapless::String<256> = String::<256>::new();
    write!(owned_string, "Bearer {api_key}")?;
    info!("Bearer token: {}", owned_string);
    Ok(owned_string)
}
