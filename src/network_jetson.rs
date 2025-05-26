use crate::{DataThread, ShutdownFn};
use anyhow::Result;
use serde::Serialize;
use std::io::{ErrorKind, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct JetsonMeasurement {
    measurement_time: usize,
    current: u32,
    voltage: u32,
}

pub(crate) fn get_data_from_jetson(
    address: String,
    data_port: u16,
    control_port: u16,
    path: PathBuf,
) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{}:{}", address, data_port))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("jetson.csv"))?;

    let mut buf = [b' '; 512];
    // starting datastream
    socket.send("go\n".as_bytes())?;
    let data_thread = thread::spawn(move || {
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
                            eprintln!("Error on receiving data: {:?}", err);
                            break;
                        }
                    }
                }
            }
            // This is a bit inefficient, but gives potential to work with the data in this program and gives slight verification
            let msg_string = String::from_utf8_lossy(&buf[..len]);
            let mut iterator = msg_string.lines().next().expect("There should be at least one line")
                .splitn(3, ',');
            wtr.serialize(JetsonMeasurement {
                measurement_time: iterator
                    .next()
                    .expect("Received no data")
                    .parse()
                    .expect("Invalid Format"),
                current: iterator
                    .next()
                    .expect("Received no current")
                    .parse()
                    .expect("Invalid Format"),
                voltage: iterator
                    .next()
                    .expect("Received no voltage")
                    .parse()
                    .expect("Invalid Format"),
            })
            .expect("Could not write Jetson measurement");
        }
        println!("Flushing Jetson data writer");
        wtr.flush().expect("Can not flush Jetson data writer");
        println!("Jetson data writer is finished");
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Jetson Interface");
            running_clone.store(false, Ordering::Relaxed);
            let mut control_connection =
                TcpStream::connect(format!("{}:{}", address, control_port))?;
            let _ = control_connection.write("stop\n".as_bytes())?;
            println!("Waiting for Data-Writer");
            Ok(())
        }),
        data_thread,
    ))
}
