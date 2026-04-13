use std::fs::File;
use crate::{DataThread, DataThreadReturnVal, ShutdownFn, PARQUET_BATCH_ROW_COUNT};
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use parquet::arrow::ArrowWriter as ParquetWriter;
use arrow::array::{ArrayBuilder, UInt16Builder};
use arrow::datatypes::{Field, Schema, DataType::UInt16};
use arrow::record_batch::RecordBatch;

pub(crate) fn get_data_from_fast_firmware(
    address: String,
    data_port: u16,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
    channel: u8,
    duration: Duration,
    sample_rate: u16,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{address}:{data_port}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_cloned = running.clone();

    let schema = Arc::new(Schema::new(vec![
        Field::new("measurement_index", UInt16, false),
        Field::new("current", UInt16, false),
    ]));
    let file = File::create(path.join("fast_firmware.parquet"))?;
    let mut wtr = ParquetWriter::try_new(file, schema.clone(), None)?;
    let mut index_array = UInt16Builder::new();
    let mut current_array = UInt16Builder::new();

    let mut buf = [b' '; 256];
    let data_thread = thread::spawn(move || -> anyhow::Result<DataThreadReturnVal> {
        while !read_start.load(Ordering::Acquire) {}

        // starting datastream
        socket.send(format!("go {} {} {}\n", channel, duration.as_micros(), sample_rate).as_bytes())?;
        while running.load(Ordering::Relaxed) {
            match socket.recv(&mut buf) {
                Ok(len) => {
                    if len != 256 {
                        running.store(false, Ordering::Relaxed);
                        if b"Finished Measurement!\n"[..] == buf[0..len] {
                            log::info!("Fast Firmware measurement complete");
                        } else {
                            log::warn!("Invalid Packet Length ({len}), stopping recording");
                        }
                        continue;
                    }
                }
                Err(err) => match err.kind() {
                    ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                        log::warn!("Could not read from Socket, stopping recording");
                        running.store(false, Ordering::Relaxed);
                        continue;
                    }
                    _ => {
                        log::error!("Error on receiving data: {err}");
                        break;
                    }
                },
            }
            let mut msg_iter = buf.chunks(4);
            //let mut packet_header = [0u8; 8];
            //packet_header.copy_from_slice(&buf[0..8]);
            //let packet_timestamp = i64::from_le_bytes(packet_header);
            for msg in &mut msg_iter {
                index_array.append_value(
                    u16::from_le_bytes([msg[0], msg[1]])
                );
                current_array.append_value(
                    u16::from_le_bytes([msg[2], msg[3]])
                );
            }
            if index_array.len() >= PARQUET_BATCH_ROW_COUNT {
                let batch = RecordBatch::try_new(schema.clone(), vec![
                    Arc::new(index_array.finish()),
                    Arc::new(current_array.finish()),
                ])?;
                wtr.write(&batch)?;
            }
        }
        log::info!("Finishing thread");
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(index_array.finish()),
            Arc::new(current_array.finish()),
        ])?;
        wtr.write(&batch)?;
        Ok(DataThreadReturnVal::ParquetWriter(wtr))
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
