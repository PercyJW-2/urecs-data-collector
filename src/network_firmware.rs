use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use http_req::request::{Authentication, Method, Request};
use http_req::uri::Uri;
use serde::{Deserialize, Serialize};
use crate::{DataThread, ShutdownFn};

#[derive(Debug, Deserialize)]
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
    #[serde(rename = "@lastPowerState")]
    last_power_state: u8,
    #[serde(rename = "@defaultPowerState")]
    default_power_state: u8,
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
    #[serde(rename = "@forceRecovery")]
    force_recovery: bool,
    #[serde(rename = "@jetsonType")]
    jetson_type: String,
    #[serde(rename = "@baseboardPosition")]
    baseboard_position: u8
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct FirmwareMeasurement {
    measurement_time: usize,
    voltage: f64,
    power: f64,
}

pub(crate) fn get_data_from_firmware(address: String, path: PathBuf) -> anyhow::Result<(ShutdownFn, DataThread)> {
    // this is hideous, but I don't know a different option
    let uri_str: &'static str = format!("https://{}/REST/node/RCU_0_BB_1_1", address).leak();
    let uri = Uri::try_from(uri_str)?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("firmware.csv"))?;

    let data_thread = thread::spawn(move || {
        let mut last_sensor_update = 0;
        while running.load(Ordering::Relaxed) {
            let mut body = Vec::new();
            let _ = Request::new(&uri)
                .authentication(Authentication::basic("admin", "admin"))
                .method(Method::GET)
                .send(&mut body);
            let body = String::from_utf8_lossy(&body);
            let node: NodeJetson = quick_xml::de::from_str(&body).expect("Could not parse node XML");
            if last_sensor_update != node.last_sensor_update {
                last_sensor_update = node.last_sensor_update;
                wtr.serialize(FirmwareMeasurement {
                    measurement_time: last_sensor_update,
                    voltage: node.voltage,
                    power: node.actual_power_usage,
                }).expect("Could not write Firmware Measurement");
                //TODO this is only for testing
                println!("Measurement: {}V, {}mW", node.voltage, node.actual_power_usage);
            }
            thread::sleep(Duration::from_millis(100));
        }
        wtr.flush().expect("Could not flush firmware data writer");
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Firmware Interface");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}