use std::io::ErrorKind;
use crate::{DataThread, ShutdownFn};
use serde::Serialize;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use crossbeam_channel::Receiver;

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
    rx: Receiver<()>
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{}:{}", address, data_port))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_cloned = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("fast_firmware.csv"))?;

    let mut buf = [b' '; 4096];
    let data_thread = thread::spawn(move || -> anyhow::Result<()> {
        rx.recv().expect("Could not receive from channel");
        
        // starting datastream
        socket.send("go\n".as_bytes())?;
        while running.load(Ordering::Relaxed) {
            let len;
            match socket.recv(&mut buf) {
                Ok(length) => {
                    len = length;
                    if len == 0 || buf[len - 1] != b'\n' {
                        continue;
                    }
                },
                Err(err) => {
                    match err.kind() { 
                        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                            continue;
                        }
                        _ => {
                            eprintln!("Error on receiving data: {}", err);
                            break;
                        }
                    }
                }
            }
            let mut msg_iter = buf.split(|c| *c == b'\n');
            for msg in &mut msg_iter {
                if msg.len() < 12 {
                    continue;
                }
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
        println!("Flushing Fast Firmware data writer");
        wtr.flush()
            .expect("Can not flush Fast Firmware data writer");
        Ok(())
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
