use crate::{DataThread, ShutdownFn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use crate::utils::is_response_valid;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NodeJetson {
    #[serde(rename = "@maxPowerUsage")]
    max_power_usage: f64,
    #[serde(rename = "@architecture")]
    architecture: String,
    #[serde(rename = "@baseboardId")]
    baseboard_id: String,
    #[serde(rename = "@voltage")]
    voltage: f64,
    #[serde(rename = "@actualNodePowerUsage")]
    actual_node_power_usage: f64,
    #[serde(rename = "@actualPowerUsage")]
    actual_power_usage: f64,
    #[serde(rename = "@state")]
    state: u8,
    #[serde(rename = "@rcuId")]
    rcu_id: String,
    #[serde(rename = "@health")]
    health: String,
    #[serde(rename = "@lastSensorUpdate")]
    last_sensor_update: usize,
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@present")]
    present: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct FirmwareMeasurement {
    measurement_time: usize,
    voltage: f64,
    power: f64,
}

pub(crate) fn get_data_from_firmware(
    address: String,
    path: PathBuf,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let uri_string = format!("https://{}/REST/node/RCU_0_BB_1_1", address);

    let client = reqwest::blocking::ClientBuilder::new()
        // TODO change to accepting the used ca-cert
        .danger_accept_invalid_certs(true)
        .build()?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("firmware.csv"))?;

    let data_thread = thread::spawn(move || -> anyhow::Result<()> {
        let mut last_sensor_update = 0;
        while running.load(Ordering::Relaxed) {
            let response = client.get(uri_string.as_str())
                .basic_auth("admin", Some("admin"))
                .send()
                .expect("failed to send firmware update request");
            if is_response_valid(&response) {
                continue;
            }
            let response = response.bytes().expect("Failed to read response");
            let body = String::from_utf8_lossy(&*response);
            let node: NodeJetson =
                quick_xml::de::from_str(&body).expect("Could not parse node XML");
            if last_sensor_update != node.last_sensor_update {
                last_sensor_update = node.last_sensor_update;
                wtr.serialize(FirmwareMeasurement {
                    measurement_time: last_sensor_update,
                    voltage: node.voltage,
                    power: node.actual_power_usage,
                })
                .expect("Could not write Firmware Measurement");
            }
            thread::sleep(Duration::from_millis(100));
        }
        wtr.flush().expect("Could not flush firmware data writer");
        Ok(())
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Firmware Interface");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread,
    ))
}
