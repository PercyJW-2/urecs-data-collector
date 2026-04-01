use std::fs::File;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn, PARQUET_BATCH_ROW_COUNT};
use serde::{Deserialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use crate::utils::is_response_valid;
use parquet::arrow::ArrowWriter as ParquetWriter;
use arrow::array::{ArrayBuilder, UInt64Builder, Float64Builder};
use arrow::datatypes::{Field, Schema, DataType::{UInt64, Float64}};
use arrow::record_batch::RecordBatch;

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

pub(crate) fn get_data_from_firmware(
    address: String,
    path: PathBuf,
    read_start: Arc<AtomicBool>
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let uri_string = format!("https://{address}/REST/node/RCU_0_BB_1_1");

    let client = reqwest::blocking::ClientBuilder::new()
        // TODO change to accepting the used ca-cert
        .danger_accept_invalid_certs(true)
        .build()?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("measurement_time", UInt64, false),
        Field::new("voltage", Float64, false),
        Field::new("power", Float64, false),
    ]));
    let file = File::create(path.join("firmware.parquet"))?;
    let mut wtr = ParquetWriter::try_new(file, schema.clone(), None)?;
    let mut time_array = UInt64Builder::new();
    let mut voltage_array = Float64Builder::new();
    let mut power_array = Float64Builder::new();

    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Acquire) {}
        
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
            let body = String::from_utf8_lossy(&response);
            let node: NodeJetson =
                quick_xml::de::from_str(&body).expect("Could not parse node XML");
            if last_sensor_update != node.last_sensor_update {
                last_sensor_update = node.last_sensor_update;
                time_array.append_value(last_sensor_update as u64);
                voltage_array.append_value(node.voltage);
                power_array.append_value(node.actual_power_usage);

                if time_array.len() >= PARQUET_BATCH_ROW_COUNT {
                    let batch = RecordBatch::try_new(schema.clone(), vec![
                        Arc::new(time_array.finish()),
                        Arc::new(voltage_array.finish()),
                        Arc::new(power_array.finish()),
                    ])?;
                    wtr.write(&batch)?;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        log::info!("Finishing thread");
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(time_array.finish()),
            Arc::new(voltage_array.finish()),
            Arc::new(power_array.finish()),
        ])?;
        wtr.write(&batch)?;
        Ok(DataThreadReturnVal::ParquetWriter(wtr))
    });
    Ok((
        Box::new(move || {
            log::info!("Shutting down Firmware Interface");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread,
    ))
}
