use crate::{DataThread, ShutdownFn};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

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

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("jetson.csv"))?;

    let mut buf = [b' '; 512];
    // starting datastream
    socket.send("go\n".as_bytes())?;
    let data_thread = thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            let len = socket.recv(&mut buf).expect("Can not receive data");
            if len == 0 || buf[len - 1] != b'\n' {
                continue;
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
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Jetson Interface");
            running_clone.store(false, Ordering::Relaxed);
            let mut control_connection =
                TcpStream::connect(format!("{}:{}", address, control_port))?;
            let _ = control_connection.write("stop\n".as_bytes())?;
            Ok(())
        }),
        data_thread,
    ))
}
