use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use serde::{Deserialize};
use crate::{DataThread, DataThreadReturnVal, ShutdownFn, PARQUET_BATCH_ROW_COUNT};
use crate::utils::is_response_valid;
use arrow::array::{ArrayBuilder, UInt64Builder, Float32Builder};
use arrow::datatypes::{Field, Schema, DataType::{UInt64, Float32}};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter as ParquetWriter;

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

pub(crate) fn get_data_from_shelly(
    address: String,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let uri_string = format!("http://{address}/rpc/Switch.GetStatus?id=0");

    let client = reqwest::blocking::ClientBuilder::new()
        .build()?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("measurement_time", UInt64, false),
        Field::new("voltage", Float32, false),
        Field::new("current", Float32, false),
        Field::new("power", Float32, false),
    ]));
    let file = File::create(path.join("shellyPlug.parquet"))?;
    let mut wtr = ParquetWriter::try_new(file, schema.clone(), None)?;
    let mut time_array = UInt64Builder::new();
    let mut voltage_array = Float32Builder::new();
    let mut current_array = Float32Builder::new();
    let mut power_array = Float32Builder::new();

    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Acquire) {}
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

            time_array.append_value(measurement_start.elapsed().as_micros() as u64);
            voltage_array.append_value(json_body.voltage);
            current_array.append_value(json_body.current);
            power_array.append_value(json_body.apower);

            if time_array.len() >= PARQUET_BATCH_ROW_COUNT {
                let batch = RecordBatch::try_new(schema.clone(), vec![
                    Arc::new(time_array.finish()),
                    Arc::new(voltage_array.finish()),
                    Arc::new(current_array.finish()),
                    Arc::new(power_array.finish())
                ])?;
                wtr.write(&batch)?;
            }
            thread::sleep(Duration::from_millis(100));
        }
        log::info!("Finishing Thread");
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(time_array.finish()),
            Arc::new(voltage_array.finish()),
            Arc::new(current_array.finish()),
            Arc::new(power_array.finish()),
        ])?;
        wtr.write(&batch)?;
        Ok(DataThreadReturnVal::WriterAndExtraFile((
            wtr,
            path.join("shellyFinalPower.txt"),
            last_consumed_energy.to_string())
        ))
    });

    Ok((
        Box::new(move || {
            log::info!("Shutting down Shelly Plug Interface");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

fn reset_shelly_plug_reading(address: String, client: &reqwest::blocking::Client) {
    let uri_string = format!("http://{address}/rpc/Switch.ResetCounters?id=0");
    client.get(uri_string.as_str())
        .send()
        .expect("failed to send plug status reset");
}