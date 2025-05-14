use crate::{DataThread, ShutdownFn};
use serde::Serialize;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct FirmwareFastMeasurement {
    measurement_time: i64,
    current: u16,
}

pub(crate) fn get_data_from_fast_firmware(
    address: String,
    data_port: u16,
    path: PathBuf,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{}:{}", address, data_port))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_cloned = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("fast_firmware.csv"))?;

    let mut buf = [b' '; 4096];
    // starting datastream
    socket.send("go\n".as_bytes())?;
    let data_thread = thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            let len = socket.recv(&mut buf).unwrap();
            if len == 0 || buf[len - 1] != b'\n' {
                continue;
            }
            let mut msg_iter = buf.split(|c| *c == b'\n');
            for msg in &mut msg_iter {
                let mut measurement_buf = [0u8; 8];
                let mut current_buf = [0u8; 2];
                measurement_buf.copy_from_slice(&msg[..8]);
                current_buf.copy_from_slice(&msg[8..10]);
                wtr.serialize(FirmwareFastMeasurement {
                    measurement_time: i64::from_le_bytes(measurement_buf),
                    current: u16::from_le_bytes(current_buf),
                })
                .expect("Could not write Fast Firmware measurement");
            }
        }
        wtr.flush()
            .expect("Can not flush Fast Firmware data writer");
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Fast Firmware Interface");
            running_cloned.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread,
    ))
}
