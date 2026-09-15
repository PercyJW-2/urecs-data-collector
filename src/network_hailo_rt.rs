use std::sync::{Arc, Barrier};
use std::time::Duration;

use anyhow::Result;
use arrow::array::{ArrayBuilder, ArrayRef, Float32Builder, UInt64Builder};
use arrow::datatypes::DataType::{Float32, UInt64};
use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatch;

use crate::network::{NetworkData, get_data_from_network};
use crate::{DataThread, ShutdownFn};

struct HailoRtData {
    schema: Arc<Schema>,
    time: UInt64Builder,
    power: Float32Builder,
    last_power: f32,
}

impl HailoRtData {
    fn new() -> Self {
        Self {
            schema: Arc::new(Schema::new(vec![
                Field::new("measurementTime", UInt64, false),
                Field::new("power", Float32, false),
            ])),
            time: UInt64Builder::new(),
            power: Float32Builder::new(),
            last_power: 0.0,
        }
    }
}

impl NetworkData for HailoRtData {
    fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    fn file_name(&self) -> &'static str {
        "hailo_rt.parquet"
    }

    fn parse_packet(&mut self, packet: &[u8]) -> Result<()> {
        let message = String::from_utf8_lossy(packet);
        let mut values = message
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received an empty HailoRT packet"))?
            .splitn(2, ',');

        self.time.append_value(
            values
                .next()
                .ok_or_else(|| anyhow::anyhow!("Received no HailoRT timestamp"))?
                .parse()?,
        );
        self.last_power = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received no HailoRT power"))?
            .parse()?;
        self.power.append_value(self.last_power);
        Ok(())
    }

    fn row_count(&self) -> usize {
        self.time.len()
    }

    fn finish_batch(&mut self) -> Result<RecordBatch> {
        Ok(RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(self.time.finish()) as ArrayRef,
                Arc::new(self.power.finish()) as ArrayRef,
            ],
        )?)
    }

    fn append_final_row(&mut self, elapsed: Duration) {
        self.time.append_value(elapsed.as_micros() as u64);
        self.power.append_value(self.last_power);
    }
}

pub(crate) fn get_data_from_hailo_rt(
    address: String,
    data_port: u16,
    control_port: u16,
    path: std::path::PathBuf,
    read_start: Arc<Barrier>,
) -> Result<(ShutdownFn, DataThread)> {
    get_data_from_network(
        address,
        data_port,
        control_port,
        path,
        read_start,
        HailoRtData::new(),
        "HailoRT",
    )
}
