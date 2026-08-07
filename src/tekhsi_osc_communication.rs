use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{thread};
use std::thread::sleep;
use std::time::Duration;
use anyhow::anyhow;
use arrow::array::Float64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use tokio::runtime::Runtime;
use log::{error, info};
use parquet::arrow::ArrowWriter;
use tekhsi_rs::errors::TekHsiError;
use tekhsi_rs::{SubscribeOptions, TekHsiClient};
use tekhsi_rs::data::{ChannelData, Waveform};
use tekhsi_rs::errors::DecodeError::NoData;
use tokio::select;
use tokio_util::sync::CancellationToken;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn};

pub(crate) fn get_data_from_tek_hsi_oscilloscope(
    address: String,
    sample_rate: u32,
    duration: Duration,
    read_start: Arc<AtomicBool>,
    path: PathBuf,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    setup_scope(sample_rate, duration)?;

    let rt = Runtime::new()?;

    let (client, symbols) =
        rt.block_on(initialize_scope(format!("{address}:5000").as_str()))?;

    let token = CancellationToken::new();
    let cloned_token = token.clone();
    //let running = Arc::new(AtomicBool::new(true));
    //let running_clone = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("current", DataType::Float64, false)
    ]));
    let file = File::create(path.join("tek_hsi.parquet"))?;
    let wtr = ArrowWriter::try_new(file, schema.clone(), None)?;

    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Relaxed) {}

        rt.block_on(async {
            let transmit_future = transmit_data(cloned_token, client, symbols[0].clone(), wtr, schema);
            //let init_future = tokio::task::spawn_blocking(move || {
            //    setup_scope(sample_rate, duration)
            //});
            let res = transmit_future.await;
            //init_future.await??;
            res
        })
            .map(DataThreadReturnVal::ParquetWriter)
    });
    Ok((
        Box::new(move || {
            info!("Shutting down TekHSI Interface");
            sleep(Duration::from_secs(2));
            token.cancel();
            Ok(())
        }),
        data_thread,
    ))
}

#[cfg(feature = "visa")]
fn setup_scope(sample_rate: u32, duration: Duration) -> anyhow::Result<()> {
    use visa_rs::prelude::*;
    use visa_rs::{AsResourceManager, ResID};
    use std::io::{BufRead, BufReader, Write};

    info!("Starting Scope Setup");
    let rm: DefaultRM = DefaultRM::new()?;
    let res_id = ResID::from_string("(TCPIP|USB)?*INSTR".parse()?).unwrap();
    info!("Using ResID {}", res_id);
    let resource = rm.find_res_list(&res_id)?;
    let resource_list: Vec<ResID> = resource.map(|res| res.unwrap()).collect();
    for res in &resource_list {
        info!("Found res {}", res);
    }
    let instr = rm.open(&resource_list[0], AccessMode::NO_LOCK, TIMEOUT_INFINITE)?;

    let mut buf_reader = BufReader::new(&instr);
    let mut buf = String::new();

    // enable correct channel
    (&instr).write_all(b"SELECT:CH1 ON; CH2 OFF; CH3 OFF; CH4 OFF")?;
    (&instr).write_all(b"SELECT:CH1?;CH2?;CH3?;CH4?")?;
    buf_reader.read_line(&mut buf)?;
    info!("Enabled Channels: {}", buf.trim_end());
    // Setup samplerate
    let memory_depth = (sample_rate as f64 * duration.as_secs_f64()) as u32;
    (&instr).write_all(
        format!(
            "HORIZONTAL:MODE MANUAL;:HORIZONTAL:SAMPLERATE {};:HORIZONTAL:RECORDLENGTH {}",
            sample_rate,
            memory_depth
        ).as_bytes()
    )?;
    (&instr).write_all(b"HORIZONTAL:SAMPLERATE?;:HORIZONTAL:RECORDLENGTH?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("Samplerate, Recordlength: {}", buf.trim_end());
    // Measure Continuously
    (&instr).write_all(b"ACQUIRE:STOPAFTER RUNSTOP")?;
    (&instr).write_all(b"ACQUIRE:STOPAFTER?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("Stop after: {}", buf.trim_end());
    // Setup Trigger
    (&instr).write_all(b"TRIGGER:A:TYPE EDGE;:TRIGGER:A:EDGE:SOURCE CH2;:TRIGGER:A:EDGE:SLOPE FALL;:TRIGGER:A:LEVEL:CH1 0.9;:TRIGGER:A:MODE NORMAL")?;
    (&instr).write_all(b"TRIGGER:A?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("Trigger Mode: {}", buf.trim_end());
    (&instr).write_all(b"HORIZONTAL:DELAY:MODE OFF;:HORIZONTAL:POSITION 0")?;
    (&instr).write_all(b"HORIZONTAL?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("Horizontal Settings: {}", buf.trim_end());

    // Start measurement
    (&instr).write_all(b"ACQUIRE:STATE RUN")?;
    (&instr).write_all(b"ACQUIRE:STATE?")?;
    buf.clear();
    buf_reader.read_line(&mut buf)?;
    info!("Acquire State: {}", buf.trim_end());
    sleep(Duration::from_secs(1));
    Ok(())
}

#[cfg(not(feature = "visa"))]
fn setup_scope(_sample_rate: u32, _duration: Duration) -> anyhow::Result<()> {
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
    running: CancellationToken,
    client: TekHsiClient,
    symbol: String,
    mut wtr: ArrowWriter<File>,
    schema: Arc<Schema>,
) -> anyhow::Result<ArrowWriter<File>> {
    let mut rx = client.subscribe(
        vec![symbol.clone()],
        SubscribeOptions {
            capacity: 16,
            download_chunk_size: 50 * 1024 * 1024,
            decode_buffer_capacity: 32,
        }
    )?;

    let mut last_acq_id: Option<u64> = None;

    info!("Starting to listen for new Data");
    loop {
        let acquisition = match select! {
        _ = running.cancelled() => Err(anyhow::anyhow!("Acquisition was stopped - This is normal behaviour")),
        res = rx.recv() => res.map_err(anyhow::Error::from),
        } {
            Ok(acquisition) => acquisition,
            Err(error) => {
                error!("{}", error);
                break;
            }
        };
        let channel_data = acquisition.get_by_symbol(symbol.as_str())
            .ok_or(TekHsiError::Decode(NoData))?;
        //info!("Received data");
        match channel_data {
            ChannelData::Waveform { acq_id, symbol: _, header: _, waveform } => {
                if let Some(last_acq_id) = last_acq_id && last_acq_id == *acq_id {
                    continue;
                }
                last_acq_id = Some(*acq_id);
                match waveform {
                    Waveform::Analog(data) => {
                        //info!("Trigger Index: {}, X-Axis-Spacing {}", data.trigger_index, data.x_axis_spacing);
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
    info!("Waiting for Disconnect");
    client.disconnect().await?;
    info!("TekHSI Disconnect");

    Ok(wtr)
}