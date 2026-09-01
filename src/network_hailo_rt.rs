use std::fs::File;
use std::io::{ErrorKind, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use anyhow::Result;
use arrow::array::{ArrayBuilder, Float32Builder, UInt64Builder};
use arrow::datatypes::{Field, Schema};
use arrow::datatypes::DataType::{Float32, UInt64};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn, PARQUET_BATCH_ROW_COUNT};

pub(crate) fn get_data_from_hailo_rt(
    address: String,
    data_port: u16,
    control_port: u16,
    path: PathBuf,
    read_start: Arc<Barrier>
) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{address}:{data_port}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    
    let schema = Arc::new(Schema::new(vec![
        Field::new("measurementTime", UInt64, false),
        Field::new("power", Float32, false)
    ]));
    let file = File::create(path.join("hailo_rt.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), None)?;
    let mut time_array = UInt64Builder::new();
    let mut power_array = Float32Builder::new();
    
    let mut last_power: f32 = 0.0;
    
    let mut buf = [b' '; 512];
    let data_thread = thread::spawn(move || {
        read_start.wait();
        let start_time = Instant::now();
        
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
                            return Err(anyhow::format_err!(err));
                        }
                    }
                }
            }
            
            let msg_string = String::from_utf8_lossy(&buf[..len]);
            let mut iterator = msg_string.lines().next().expect("There should be at least one line")
                .splitn(2, ',');
            time_array.append_value(iterator.next().expect("Received no data").parse()?);
            last_power = iterator.next().expect("Received no power").parse()?;
            if time_array.len() >= PARQUET_BATCH_ROW_COUNT {
                let batch = RecordBatch::try_new(schema.clone(), vec![
                    Arc::new(time_array.finish()),
                    Arc::new(power_array.finish()),
                ])?;
                writer.write(&batch)?;
            }
        }
        log::info!("Finishing thread");
        time_array.append_value(start_time.elapsed().as_micros() as u64);
        power_array.append_value(last_power);
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(time_array.finish()),
            Arc::new(power_array.finish()),
        ])?;
        writer.write(&batch)?;
        Ok(DataThreadReturnVal::ParquetWriter(writer))
    });
    Ok((
        Box::new(move || {
            log::info!("Shutting down HailoRT Interface");
            running_clone.store(false, Ordering::Relaxed);
            let mut control_connection = 
                TcpStream::connect(format!("{address}:{control_port}"))?;
            let _ = control_connection.write("stop\n".as_bytes())?;
            log::info!("Waiting for HailoRT Data-Writer");
            Ok(())
        }),
        data_thread,
    ))
}