use crate::{DataThread, DataThreadReturnVal, ShutdownFn};
use serde::Serialize;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct FirmwareFastMeasurement {
    //packet_timestamp: i64,
    //measurement_time: u16,
    current: f32,
}

pub(crate) fn get_data_from_fast_firmware(
    address: String,
    data_port: u16,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{address}:{data_port}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_cloned = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("fast_firmware.csv"))?;

    let mut buf = [b' '; 20];
    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Acquire) {}

        // starting datastream
        socket.send("go\n".as_bytes())?;
        while running.load(Ordering::Relaxed) {
            let len;
            match socket.recv(&mut buf) {
                Ok(length) => {
                    len = length;
                    if len != 20 {
                        log::warn!("Invalid Packet Length ({len}), skipping");
                        continue;
                    }
                }
                Err(err) => match err.kind() {
                    ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                        log::warn!("Could not read from Socket, trying again...");
                        continue;
                    }
                    _ => {
                        log::error!("Error on receiving data: {err}");
                        break;
                    }
                },
            }
            let mut msg_iter = buf.chunks(2);
            //let mut packet_header = [0u8; 8];
            //packet_header.copy_from_slice(&buf[0..8]);
            //let packet_timestamp = i64::from_le_bytes(packet_header);
            for msg in &mut msg_iter {
                let raw_current = u16::from_le_bytes([msg[0], msg[1]]);
                // ADC has values between 4095 and 0, INA225 has gain of 25V/V, Current Shunt has value of 0.02 Ohm
                // raw_val / 4096 / 25 / 0.02 = current <=> raw_val / 2048
                let current = f32::from(raw_current) / 2048f32;
                wtr.serialize(FirmwareFastMeasurement {
                    //packet_timestamp,
                    //measurement_time: u16::from_le_bytes([msg[0], msg[1]]),
                    current,
                })
                .expect("Could not write Fast Firmware measurement");
            }
        }
        log::info!("Finishing thread");
        Ok(DataThreadReturnVal::CsvWriter(wtr))
    });
    Ok((
        Box::new(move || {
            log::info!("Shutting down Fast Firmware Interface");
            running_cloned.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread,
    ))
}
