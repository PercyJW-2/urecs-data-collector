use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use pico_sdk::prelude::*;
use anyhow::Result;
use crossbeam_channel::Receiver;
use crate::{ShutdownFn, DataThread};

pub(crate) fn get_data_from_usb_osc(path: PathBuf, rx: Receiver<()>) -> Result<(ShutdownFn, DataThread)> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let wtr_handler = CSVHandler::new(path.join("usb_osc_data.csv"), PicoChannel::A)?;
    let instrument_wrapper = USBInstrumentWrapper::new(Arc::new(wtr_handler))?;

    let data_thread = thread::spawn(move || -> Result<()> {

        rx.recv().expect("Could not receive from channel");

        instrument_wrapper.start(1_000)?;
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

        stream_device.enable_channel(PicoChannel::A, PicoRange::X1_PROBE_5V, PicoCoupling::AC);
        stream_device.new_data.subscribe(csv_handler.clone());

        Ok(Self {
            _csv_handler: csv_handler,
            stream_device,
        })
    }

    fn start(&self, sample_rate: u32) -> Result<()> {
        self.stream_device.start(sample_rate)?;
        Ok(())
    }

    fn stop(&self) {
        self.stream_device.stop();
    }
}

struct CSVHandler {
    csv_writer: csv::Writer<File>,
    channel: PicoChannel,
}

impl CSVHandler {
    fn new(path: PathBuf, channel: PicoChannel) -> Result<Self> {
        let wtr = csv::Writer::from_path(path)?;
        Ok(Self {
            csv_writer: wtr,
            channel,
        })
    }
}

impl NewDataHandler for CSVHandler {
    fn handle_event(&self, value: &StreamingEvent) {
        for sample in &value.channels[&self.channel].samples {

        };
        todo!()
    }
}

impl Drop for CSVHandler {
    fn drop(&mut self) {
        self.csv_writer.flush().unwrap();
    }
}