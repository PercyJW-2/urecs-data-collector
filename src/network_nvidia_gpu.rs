use std::sync::Arc;
use std::time::Duration;
use arrow::array::{ArrayBuilder, ArrayRef, Float64Builder, RecordBatch, UInt32Builder};
use arrow::datatypes::{Field, Schema};
use arrow::datatypes::DataType::{Float64, UInt32};
use crate::network::{get_data_from_network, NetworkData};
use crate::{DataThread, ShutdownFn};

struct NvidiaGpuData {
    schema: Arc<Schema>,
    time: Float64Builder,
    power: UInt32Builder,
    last_power: u32,
}

impl NvidiaGpuData {
    fn new() -> Self {
        Self {
            schema: Arc::new(Schema::new(vec![
                Field::new("measurementTime", Float64, false),
                Field::new("power", UInt32, false),
            ])),
            time: Float64Builder::new(),
            power: UInt32Builder::new(),
            last_power: 0,
        }
    }
}

impl NetworkData for NvidiaGpuData {
    fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    fn file_name(&self) -> &'static str {
        "nvidia_gpu.parquet"
    }

    fn parse_packet(&mut self, packet: &[u8]) -> anyhow::Result<()> {
        let message = String::from_utf8_lossy(packet);
        let mut values = message
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received an empty Nvidia GPU packet"))?
            .splitn(2, ',');

        self.time.append_value(
            values
                .next()
                .ok_or_else(|| anyhow::anyhow!("Received no Nvidia GPU timestamp"))?
                .parse()?,
        );
        self.last_power = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received no Nvidia GPU power"))?
            .parse()?;
        self.power.append_value(self.last_power);

        Ok(())
    }

    fn row_count(&self) -> usize {
        self.time.len()
    }

    fn finish_batch(&mut self) -> anyhow::Result<RecordBatch> {
        Ok(RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(self.time.finish()) as ArrayRef,
                Arc::new(self.power.finish()) as ArrayRef,
            ],
        )?)
    }

    fn append_final_row(&mut self, elapsed: Duration) {
        self.time.append_value(elapsed.as_secs_f64());
        self.power.append_value(self.last_power);
    }
}

pub(crate) fn get_data_from_nvidia_gpu(
    address: String,
    data_port: u16,
    control_port: u16,
    path: std::path::PathBuf,
    read_start: Arc<std::sync::Barrier>,
) -> anyhow::Result<(ShutdownFn, DataThread)> {
    get_data_from_network(
        address,
        data_port,
        control_port,
        path,
        read_start,
        NvidiaGpuData::new(),
        "Nvidia GPU",
    )
}