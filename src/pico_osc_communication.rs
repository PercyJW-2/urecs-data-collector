use crate::{DataThread, DataThreadReturnVal, ShutdownFn};
use anyhow::Result;
use pico_sdk::prelude::*;
use serde::Serialize;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use log::info;
use pico_sdk::common::{PicoExtraOperations, PicoSigGenTrigSource, PicoSweepType, PicoWaveType, SetSigGenBuiltInV2Properties, SweepShotCount};

pub(crate) fn get_data_from_usb_osc(path: PathBuf, read_start: Arc<AtomicBool>, sample_rate: u32, start_func_gen: bool) -> Result<(ShutdownFn, DataThread)> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let wtr_handler = CSVHandler::new(path.join("usb_osc_data.csv"))?;
    let mut instrument_wrapper = USBInstrumentWrapper::new(Arc::new(wtr_handler), start_func_gen)?;

    let data_thread = thread::spawn(move || -> Result<DataThreadReturnVal> {

        while !read_start.load(Ordering::Acquire) {}

        //instrument_wrapper.start(50_000_000)?;
        instrument_wrapper.start(sample_rate)?;
        while running.load(Ordering::Relaxed) {
            //thread::sleep(std::time::Duration::from_millis(1));
        }
        instrument_wrapper.stop();
        info!("Finishing Thread");
        Ok(DataThreadReturnVal::Instrument(instrument_wrapper))
    });

    Ok((
        Box::new(move || {
            println!("Shutting down USBOsc");
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

pub(crate) struct USBInstrumentWrapper {
    pub(crate) csv_handler: Arc<CSVHandler>,
    stream_device: PicoStreamingDevice
}

impl USBInstrumentWrapper {
    fn new(csv_handler: Arc<CSVHandler>, start_func_gen: bool) -> Result<Self> {
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
        
        stream_device.enable_channel(PicoChannel::A, PicoRange::X1_PROBE_200MV, PicoCoupling::DC, -0.2);
        stream_device.enable_channel(PicoChannel::B, PicoRange::X1_PROBE_10V, PicoCoupling::DC, 15.0);
        stream_device.new_data.subscribe(csv_handler.clone());

        Ok(Self {
            csv_handler,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct UsbOscMeasurement {
    voltage: f64,
    current: f64,
}

pub(crate) struct CSVHandler {
    pub(crate) csv_writer: Mutex<csv::Writer<File>>,
}

impl CSVHandler {
    fn new(path: PathBuf) -> Result<Self> {
        let wtr = csv::Writer::from_path(path)?;
        Ok(Self {
            csv_writer: Mutex::new(wtr),
        })
    }
}

impl NewDataHandler for CSVHandler {
    fn handle_event(&self, value: &StreamingEvent) {
        let mut wtr_lock = self.csv_writer.lock().expect("Could not lock the mutex");
        let channel_a_data = &value.channels[&PicoChannel::A].scale_samples();
        let channel_b_data = &value.channels[&PicoChannel::B].scale_samples();
        channel_a_data.iter().zip(channel_b_data.iter()).for_each(|(channel_a, channel_b)| {
            //println!("{}", *channel_b);
            wtr_lock.serialize(UsbOscMeasurement {
                current: (*channel_a + 0.2) * 10., // value can be used directly, as 1V algins to 1A
                voltage: *channel_b + 15.0, // voltage needs to be negated, as it is measured reversely
            }).expect("Could not serialize USB Osc measurement");
        });
    }
}
