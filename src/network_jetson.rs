use std::fs::File;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn, PARQUET_BATCH_ROW_COUNT};
use anyhow::Result;
use arrow::datatypes::{Field, Schema};
use parquet::arrow::ArrowWriter as ParquetWriter;
use std::io::{ErrorKind, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use arrow::array::{ArrayBuilder, ArrayRef, UInt32Builder, UInt64Builder};
use arrow::datatypes::DataType::{UInt32, UInt64};
use arrow::record_batch::RecordBatch;

pub(crate) fn get_data_from_jetson(
    address: String,
    data_port: u16,
    control_port: u16,
    path: PathBuf,
    read_start: Arc<Barrier>,
) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{address}:{data_port}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("measurementTime", UInt64, false),
        Field::new("current", UInt32, false),
        Field::new("voltage", UInt32, false),
    ]));
    let file = File::create(path.join("jetson.parquet"))?;
    let mut writer = ParquetWriter::try_new(file, schema.clone(), None)?;
    let mut time_array = UInt64Builder::new();
    let mut current_array = UInt32Builder::new();
    let mut voltage_array = UInt32Builder::new();

    let mut last_current: u32 = 0;
    let mut last_voltage: u32 = 0;

    let mut buf = [b' '; 512];
    let data_thread = thread::spawn(move || -> Result<DataThreadReturnVal> {
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
            // This is a bit inefficient, but gives potential to work with the data in this program and gives slight verification
            let msg_string = String::from_utf8_lossy(&buf[..len]);
            let mut iterator = msg_string.lines().next().expect("There should be at least one line")
                .splitn(3, ',');
            time_array.append_value(iterator.next().expect("Received no data").parse()?);
            last_current = iterator.next().expect("Received no current").parse()?;
            current_array.append_value(last_current);
            last_voltage = iterator.next().expect("Received no voltage").parse()?;
            voltage_array.append_value(last_voltage);
            if time_array.len() >= PARQUET_BATCH_ROW_COUNT {
                let batch = RecordBatch::try_new(schema.clone(), vec![
                    Arc::new(time_array.finish()),
                    Arc::new(current_array.finish()) as ArrayRef,
                    Arc::new(voltage_array.finish()) as ArrayRef,
                ])?;
                writer.write(&batch)?
            }
        }
        log::info!("Finishing thread");
        time_array.append_value(start_time.elapsed().as_micros() as u64);
        current_array.append_value(last_current);
        voltage_array.append_value(last_voltage);
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(time_array.finish()),
            Arc::new(current_array.finish()),
            Arc::new(voltage_array.finish()),
        ])?;
        writer.write(&batch)?;
        Ok(DataThreadReturnVal::ParquetWriter(writer))
    });
    Ok((
        Box::new(move || {
            log::info!("Shutting down Jetson Interface");
            running_clone.store(false, Ordering::Relaxed);
            let mut control_connection =
                TcpStream::connect(format!("{address}:{control_port}"))?;
            let _ = control_connection.write("stop\n".as_bytes())?;
            log::info!("Waiting for Jetson Data-Writer");
            Ok(())
        }),
        data_thread,
    ))
}
