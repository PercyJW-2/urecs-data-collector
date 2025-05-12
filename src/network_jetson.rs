use crate::{DataThread, ShutdownFn};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct JetsonMeasurement {
    measurement_time: usize,
    current: u32,
    voltage: u32,
}

pub(crate) fn get_data_from_jetson(address: String, data_port: u16, control_port: u16, path: PathBuf) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", data_port))?;
    socket.connect(format!("{}:{}", address, data_port))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
   
    let mut wtr = csv::Writer::from_path(path.join("jetson.csv"))?; 
   
    let mut buf = [' ' as u8; 512];
    // starting datastream
    socket.send("go\n".as_bytes())?;
    let data_thread = thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            let len = socket.recv(&mut buf).expect("Can not receive data");
            if len == 0 { 
                continue;
            }
            let iterator = String::from_utf8_lossy(&buf[..len]).splitn(3, ',');
            /*JetsonMeasurement {
                measurement_time: iterator.next().expect(),
                
            }*/
        }
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Jetson Interface");
            let mut control_connection = TcpStream::connect(format!("{}:{}", address, control_port))?;
            control_connection.write("stop\n".as_bytes())?;
            running_clone.store(false, Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

