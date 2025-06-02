use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, thread};
use std::time::{Duration, Instant};
use reqwest::blocking::Response;
use serde::{Deserialize, Serialize};
use crate::{DataThread, ShutdownFn};
use crate::utils::is_response_valid;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SwitchStatus {
    id: u8,
    source: String,
    output: bool,
    apower: f32,
    voltage: f32,
    current: f32,
    aenergy: Aenergy,
    temperature: Temperature,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Aenergy {
    total: f32,
    by_minute: [f32; 3],
    minute_ts: u64
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Temperature {
    #[serde(rename = "tC")]
    t_c: f32,
    #[serde(rename = "tF")]
    t_f: f32
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct ShellyPlugMeasurement {
    measurement_time: u128,
    voltage: f32,
    current: f32,
    power: f32
}

pub(crate) fn get_data_from_shelly(
    address: String,
    path: PathBuf,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let uri_string = format!("http://{}/rpc/Switch.GetStatus?id=0", address);

    let client = reqwest::blocking::ClientBuilder::new()
        .build()?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("shellyPlug.csv"))?;

    let data_thread = thread::spawn(move || {
        reset_shelly_plug_reading(address, &client);
        let mut last_consumed_energy: f32 = 0.0;
        let measurement_start = Instant::now();
        while running.load(Ordering::Relaxed) {
            let response = client.get(uri_string.as_str())
                .send()
                .expect("failed to send plug status request");
            if is_response_valid(&response) { continue; }
            let json_body: SwitchStatus = response.json().expect("failed to parse plug status response");
            if last_consumed_energy == json_body.aenergy.total {
                continue;
            }
            last_consumed_energy = json_body.aenergy.total;

            wtr.serialize(ShellyPlugMeasurement {
                measurement_time: measurement_start.elapsed().as_micros(),
                voltage: json_body.voltage,
                current: json_body.current,
                power: json_body.apower,
            })
            .expect("Could not write Shelly Plug Measurement");

            thread::sleep(Duration::from_millis(100));
        }
        wtr.flush().expect("Could not flush firmware data writer");
        fs::write(path.join("shellyFinalPower.txt"), last_consumed_energy.to_string())
            .expect("failed to write last consumed energy file");
    });

    Ok((
        Box::new(move || {
            println!("Shutting down Shelly Plug Interface");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

fn reset_shelly_plug_reading(address: String, client: &reqwest::blocking::Client) {
    let uri_string = format!("http://{}/rpc/Switch.ResetCounters?id=0", address);
    client.get(uri_string.as_str())
        .send()
        .expect("failed to send plug status reset");
}