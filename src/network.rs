use std::fs::File;
use std::io::{ErrorKind, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use crate::{DataThread, DataThreadReturnVal, PARQUET_BATCH_ROW_COUNT, ShutdownFn};

pub(crate) trait NetworkData: Send + 'static {
    fn schema(&self) -> Arc<Schema>;
    fn file_name(&self) -> &'static str;
    fn parse_packet(&mut self, packet: &[u8]) -> Result<()>;
    fn row_count(&self) -> usize;
    fn finish_batch(&mut self) -> Result<RecordBatch>;
    fn append_final_row(&mut self, elapsed: Duration);
}

pub(crate) fn get_data_from_network<D: NetworkData>(
    address: String,
    data_port: u16,
    control_port: u16,
    path: impl AsRef<Path>,
    read_start: Arc<Barrier>,
    mut data: D,
    interface_name: &'static str,
) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{address}:{data_port}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let schema = data.schema();
    let file = File::create(path.as_ref().join(data.file_name()))?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), None)?;

    let data_thread = thread::spawn(move || -> Result<DataThreadReturnVal> {
        read_start.wait();
        let start_time = Instant::now();
        let mut buf = [b' '; 512];

        socket.send(b"go\n")?;
        while running.load(Ordering::Relaxed) {
            let len = match socket.recv(&mut buf) {
                Ok(length) if length > 0 && buf[length - 1] == b'\n' => length,
                Ok(_) => continue,
                Err(err) if matches!(err.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                    continue;
                }
                Err(err) => return Err(err.into()),
            };

            data.parse_packet(&buf[..len])?;
            if data.row_count() >= PARQUET_BATCH_ROW_COUNT {
                writer.write(&data.finish_batch()?)?;
            }
        }

        log::info!("Finishing {interface_name} thread");
        data.append_final_row(start_time.elapsed());
        if data.row_count() > 0 {
            writer.write(&data.finish_batch()?)?;
        }
        Ok(DataThreadReturnVal::ParquetWriter(writer))
    });

    Ok((
        Box::new(move || {
            log::info!("Shutting down {interface_name} interface");
            running_clone.store(false, Ordering::Relaxed);
            let mut control_connection = TcpStream::connect(format!("{address}:{control_port}"))?;
            control_connection.write_all(b"stop\n")?;
            log::info!("Waiting for {interface_name} data writer");
            Ok(())
        }),
        data_thread,
    ))
}
