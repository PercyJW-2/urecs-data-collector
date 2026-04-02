use crate::{DataThread, DataThreadReturnVal, OscilloscopeMsmtType, ShutdownFn};
use anyhow::Result;
use pico_sdk::prelude::*;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use log::info;
use parquet::arrow::ArrowWriter as ParquetWriter;
use arrow::array::{Float64Array};
use arrow::datatypes::{Field, Schema, DataType::Float64};
use arrow::record_batch::RecordBatch;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use pico_sdk::common::{PicoExtraOperations, PicoSigGenTrigSource, PicoSweepType, PicoWaveType, SetSigGenBuiltInV2Properties, SweepShotCount};

pub(crate) fn get_data_from_usb_osc(path: PathBuf, read_start: Arc<AtomicBool>, sample_rate: u32, start_func_gen: bool, msmt_type: OscilloscopeMsmtType) -> Result<(ShutdownFn, DataThread)> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let wtr_handler = ParquetHandler::new(path, msmt_type.clone())?;
    let mut instrument_wrapper = USBInstrumentWrapper::new(Arc::new(wtr_handler), start_func_gen, msmt_type)?;

    let data_thread = thread::spawn(move || -> Result<DataThreadReturnVal> {

        while !read_start.load(Ordering::Acquire) {}

        instrument_wrapper.start(sample_rate)?;
        while running.load(Ordering::Relaxed) {
            //thread::sleep(std::time::Duration::from_millis(1));
        }
        instrument_wrapper.stop();
        info!("Finishing Thread");
        let unused = instrument_wrapper.parquet_handler.parquet_writer.lock().expect("Could not lock ParquetWriter");
        drop(unused);
        Ok(DataThreadReturnVal::Instrument(instrument_wrapper))
    });

    Ok((
        Box::new(move || {
            info!("Shutting down USBOsc");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

pub(crate) struct USBInstrumentWrapper {
    pub(crate) parquet_handler: Arc<ParquetHandler>,
    stream_device: PicoStreamingDevice
}

impl USBInstrumentWrapper {
    fn new(parquet_handler: Arc<ParquetHandler>, start_func_gen: bool, msmt_type: OscilloscopeMsmtType) -> Result<Self> {
        let enumerator = DeviceEnumerator::new();
        let enum_device = enumerator
            .enumerate()
            .into_iter()
            .flatten()
            .next()
            .expect("No device enumerated");
        let device = enum_device.open()?;
        let stream_device = device.into_streaming_device();

        if start_func_gen {
            stream_device.set_sig_gen_built_in_v2(SetSigGenBuiltInV2Properties {
                offset_voltage: 1_000_000,
                pk_to_pk: 900_000,
                wave_type: PicoWaveType::Sine,
                start_frequency: 5_000f64,
                stop_frequency: 5_000f64,
                increment: 0.0,
                dwell_time: 0.0,
                sweep_type: PicoSweepType::Up,
                extra_operations: PicoExtraOperations::Off,
                sweeps_shots: SweepShotCount::ContinuousShots,
                trig_type: Default::default(),
                trig_source: PicoSigGenTrigSource::None,
                ext_in_threshold: 0,
            })?;
        }

        let (channel_a_range, channel_a_offset) = match msmt_type {
            OscilloscopeMsmtType::CurrentRanger => (PicoRange::X1_PROBE_2V, -2.0),
            OscilloscopeMsmtType::UCurrent => (PicoRange::X1_PROBE_200MV, -0.2),
        };
        stream_device.enable_channel(PicoChannel::A, channel_a_range, PicoCoupling::DC, channel_a_offset);
        stream_device.enable_channel(PicoChannel::B, PicoRange::X1_PROBE_10V, PicoCoupling::DC, -15.0);
        stream_device.new_data.subscribe(parquet_handler.clone());

        Ok(Self {
            parquet_handler,
            stream_device,
        })
    }

    fn start(&mut self, sample_rate: u32) -> Result<()> {
        self.stream_device.start(sample_rate)?;
        Ok(())
    }

    fn stop(&self) {
        self.stream_device.stop();
    }
}

#[derive(Debug)]
pub(crate) struct ParquetHandler {
    pub(crate) parquet_writer: Mutex<ParquetWriter<File>>,
    pub(crate) schema: Arc<Schema>,
    data_multiplication_factor: f64,
    data_offset_factor: f64,
}

impl ParquetHandler {
    fn new(path: PathBuf, msmt_type: OscilloscopeMsmtType) -> Result<Self> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("voltage", Float64, false),
            Field::new("current", Float64, false),
        ]));
        let file = File::create(path.join("usb_osc_data.parquet"))?;
        let wtr_properties = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::try_new(15)?))
            .build();
        let wtr = ParquetWriter::try_new(file, schema.clone(), Some(wtr_properties))?;
        let (data_multiplication_factor, data_offset_factor) = match msmt_type {
            OscilloscopeMsmtType::CurrentRanger => (1., 2.),
            OscilloscopeMsmtType::UCurrent => (10., 0.2)
        };
        Ok(Self {
            parquet_writer: Mutex::new(wtr),
            schema,
            data_multiplication_factor,
            data_offset_factor,
        })
    }

    pub(crate) fn flush_and_close(self) -> Result<()> {
        let mut wtr = self.parquet_writer.into_inner().expect("Parquet writer poisoned, other Errors not compatible with Sync Trait");
        wtr.flush()?;
        wtr.close()?;
        Ok(())
    }
}

impl NewDataHandler for ParquetHandler {
    fn handle_event(&self, value: &StreamingEvent) {
        let mut wtr_lock = self.parquet_writer.lock().expect("Could not lock the mutex");
        let current_data: Float64Array = value.channels[&PicoChannel::A].scale_samples().iter().map(|crnt| {
            (*crnt + self.data_offset_factor) * self.data_multiplication_factor
        }).collect();
        let voltage_data: Float64Array = value.channels[&PicoChannel::B].scale_samples().iter().map(|vltg| {
            *vltg + 15.0
        }).collect();
        let batch = RecordBatch::try_new(self.schema.clone(), vec![
            Arc::new(voltage_data),
            Arc::new(current_data)
        ]).expect("Could not create record batch");
        wtr_lock.write(&batch).expect("Could not write batch");
    }
}
