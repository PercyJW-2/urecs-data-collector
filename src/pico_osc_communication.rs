use crate::{DataThread, ShutdownFn};
use anyhow::Result;
use crossbeam_channel::Receiver;
use pico_sdk::prelude::*;
use serde::Serialize;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

pub(crate) fn get_data_from_usb_osc(path: PathBuf, rx: Receiver<()>) -> Result<(ShutdownFn, DataThread)> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let wtr_handler = CSVHandler::new(path.join("usb_osc_data.csv"))?;
    let mut instrument_wrapper = USBInstrumentWrapper::new(Arc::new(wtr_handler))?;

    let data_thread = thread::spawn(move || -> Result<()> {

        rx.recv().expect("Could not receive from channel");

        instrument_wrapper.start(50_000_000)?;
        //instrument_wrapper.start(1_000)?;
        while running.load(std::sync::atomic::Ordering::Relaxed) {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        instrument_wrapper.stop();
        Ok(())
    });

    Ok((
        Box::new(move || {
            println!("Shutting down USBOsc");
            running_clone.store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

struct USBInstrumentWrapper {
    _csv_handler: Arc<CSVHandler>,
    stream_device: PicoStreamingDevice
}

impl USBInstrumentWrapper {
    fn new(csv_handler: Arc<CSVHandler>) -> Result<Self> {
        let enumerator = DeviceEnumerator::new();
        let enum_device = enumerator
            .enumerate()
            .into_iter()
            .flatten()
            .next()
            .expect("No device enumerated");
        let device = enum_device.open()?;
        let stream_device = device.into_streaming_device();

        stream_device.enable_channel(PicoChannel::A, PicoRange::X1_PROBE_5V, PicoCoupling::DC);
        stream_device.enable_channel(PicoChannel::B, PicoRange::X1_PROBE_20V, PicoCoupling::DC);
        stream_device.new_data.subscribe(csv_handler.clone());

        Ok(Self {
            _csv_handler: csv_handler,
            stream_device,
        })
    }

    fn start(&mut self, sample_rate: u32) -> Result<()> {
        self.stream_device.start(sample_rate)?;
        self._csv_handler.start();
        Ok(())
    }

    fn stop(&self) {
        self.stream_device.stop();
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct UsbOscMeasurement {
    measurement_timestamp: u128,
    sample_index: usize,
    voltage: f64,
    current: f64,
}

struct CSVHandler {
    csv_writer: Mutex<csv::Writer<File>>,
    start_time: Mutex<Instant>,
}

impl CSVHandler {
    fn new(path: PathBuf) -> Result<Self> {
        let wtr = csv::Writer::from_path(path)?;
        Ok(Self {
            csv_writer: Mutex::new(wtr),
            start_time: Mutex::new(Instant::now())
        })
    }

    fn start(&self) {
        let mut start_time_lock = self.start_time.lock().expect("Could not lock the mutex");
        *start_time_lock = Instant::now();
    }
}

impl NewDataHandler for CSVHandler {
    fn handle_event(&self, value: &StreamingEvent) {
        let current_time = self.start_time.lock().expect("Could not lock the mutex").elapsed();
        let mut wtr_lock = self.csv_writer.lock().expect("Could not lock the mutex");
        let channel_a_data = &value.channels[&PicoChannel::A].scale_samples();
        let channel_b_data = &value.channels[&PicoChannel::B].scale_samples();
        channel_a_data.iter().zip(channel_b_data.iter()).enumerate().for_each(|(idx, (channel_a, channel_b))| {
            //println!("{}", *channel_b);
            wtr_lock.serialize(UsbOscMeasurement {
                measurement_timestamp: current_time.as_micros(),
                sample_index: idx,
                current: *channel_a, // value can be used directly, as 1V algins to 1A
                voltage: -*channel_b, // voltage needs to be negated, as it is measured reversely
            }).expect("Could not serialize USB Osc measurement");
        });
    }
}

impl Drop for CSVHandler {
    fn drop(&mut self) {
        self.csv_writer.lock().expect("could not acquire lock").flush().unwrap();
    }
}