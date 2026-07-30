use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use anyhow::anyhow;
use arrow::array::Float64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use futures::executor::block_on;
use log::{info};
use parquet::arrow::ArrowWriter;
use tekhsi_rs::errors::TekHsiError;
use tekhsi_rs::{SubscribeOptions, TekHsiClient};
use tekhsi_rs::data::{ChannelData, Waveform};
use tekhsi_rs::errors::DecodeError::NoData;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn};

pub(crate) fn get_data_from_tek_hsi_oscilloscope(
    address: String,
    sample_rate: u32,
    read_start: Arc<AtomicBool>,
    path: PathBuf,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    setup_scope(sample_rate)?;

    let (client, symbols) =
        block_on(initialize_scope(format!("{address}:5000").as_str()))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("current", DataType::Float64, false)
    ]));
    let file = File::create(path.join("tek_hsi.parquet"))?;
    let wtr = ArrowWriter::try_new(file, schema.clone(), None)?;

    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Relaxed) {}

        block_on(transmit_data(running_clone, client, symbols[0].clone(), wtr, schema))
            .map(DataThreadReturnVal::ParquetWriter)
    });
    Ok((
        Box::new(move || {
            log::info!("Shutting down TekHSI Interface");
            running.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread,
    ))
}

#[cfg(feature = "visa")]
fn setup_scope(sample_rate: u32) -> anyhow::Result<()> {
    use visa_rs::prelude::*;
    use visa_rs::{AsResourceManager, ResID};
    use std::io::{BufRead, BufReader, Write};

    let rm: DefaultRM = DefaultRM::new()?;
    let res_id = ResID::from_string("?*".parse()?).unwrap();
    let resource = rm.find_res(&res_id)?;
    let instr = rm.open(&resource, AccessMode::NO_LOCK, TIMEOUT_INFINITE)?;

    let mut buf_reader = BufReader::new(&instr);
    let mut buf = String::new();

    // enable correct channel
    (&instr).write_all(b"SELECT:CH1 ON; CH2 OFF; CH3 OFF; CH4 OFF")?;
    (&instr).write_all(b"SELECT:CH1?;CH2?;CH3?;CH4?")?;
    buf_reader.read_line(&mut buf)?;
    info!("{buf}");
    // Setup samplerate
    (&instr).write_fmt(
        format_args!(
            "HORIZONTAL:MODE MANUAL;:HORIZONTAL:SAMPLERATE {};:HORIZONTAL:RECORDLENGTH {}",
            sample_rate,
            1_000_000
        )
    )?;
    (&instr).write_all(b"HORIZONTAL:SAMPLERATE?;:HORIZONTAL:RECORDLENGTH?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("{buf}");
    // Measure Continuously
    (&instr).write_all(b"ACQUIRE:STOPAFTER SEQUENCE")?;
    (&instr).write_all(b"ACQUIRE:STOPAFTER?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("{buf}");

    // Start measurement
    (&instr).write_all(b"ACQUIRE:STATE RUN")?;
    Ok(())
}

#[cfg(not(feature = "visa"))]
fn setup_scope(_sample_rate: u32) -> anyhow::Result<()> {
    use log::warn;
    warn!("Visa Feature is not enabled, Automatic Scope setup will not be used");
    Ok(())
}

async fn initialize_scope(address: &str) -> Result<(TekHsiClient, Vec<String>), TekHsiError> {
    let client = TekHsiClient::connect(address).await?;
    let symbols = client.list_available_symbols().await?;
    println!("{:?}", symbols);
    Ok((client, symbols))
}

async fn transmit_data(
    running: Arc<AtomicBool>,
    client: TekHsiClient,
    symbol: String,
    mut wtr: ArrowWriter<File>,
    schema: Arc<Schema>,
) -> anyhow::Result<ArrowWriter<File>> {
    let mut rx = client.subscribe(
        vec![symbol.clone()],
        SubscribeOptions {
            capacity: 16,
            download_chunk_size: 4_194_304,
            decode_buffer_capacity: 32,
        }
    )?;

    while running.load(Ordering::Relaxed) && let Ok(acquisition) = rx.recv().await {
        let channel_data = acquisition.get_by_symbol(symbol.as_str())
            .ok_or(TekHsiError::Decode(NoData))?;
        match channel_data {
            ChannelData::Waveform { acq_id: _, symbol: _, header: _, waveform } => {
                match waveform {
                    Waveform::Analog(data) => {
                        let value_iter = data.iter_normalized_values();
                        let current_data: Float64Array = value_iter.collect();
                        let batch = RecordBatch::try_new(
                            schema.clone(),
                            vec![Arc::new(current_data)]
                        )?;
                        wtr.write(&batch)?;
                    }
                    Waveform::Digital(_) | Waveform::Iq(_) =>
                        return Err(anyhow!("Digital and Complex Waveforms are not supported"))
                }
            }
            ChannelData::DecodeError { symbol: _, header: _, error } =>
                return Err(anyhow!(error.clone())),
            ChannelData::AcquisitionError { symbol: _, error } =>
                return Err(anyhow!(error.clone()))
        }
    }

    Ok(wtr)
}