use std::sync::{Arc, Barrier};
use std::time::Duration;

use anyhow::Result;
use arrow::array::{ArrayBuilder, ArrayRef, UInt32Builder, UInt64Builder};
use arrow::datatypes::DataType::{UInt32, UInt64};
use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatch;

use crate::network::{NetworkData, get_data_from_network};
use crate::{DataThread, ShutdownFn};

struct JetsonData {
    schema: Arc<Schema>,
    time: UInt64Builder,
    current: UInt32Builder,
    voltage: UInt32Builder,
    last_current: u32,
    last_voltage: u32,
}

impl JetsonData {
    fn new() -> Self {
        Self {
            schema: Arc::new(Schema::new(vec![
                Field::new("measurementTime", UInt64, false),
                Field::new("current", UInt32, false),
                Field::new("voltage", UInt32, false),
            ])),
            time: UInt64Builder::new(),
            current: UInt32Builder::new(),
            voltage: UInt32Builder::new(),
            last_current: 0,
            last_voltage: 0,
        }
    }
}

impl NetworkData for JetsonData {
    fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    fn file_name(&self) -> &'static str {
        "jetson.parquet"
    }

    fn parse_packet(&mut self, packet: &[u8]) -> Result<()> {
        let message = String::from_utf8_lossy(packet);
        let mut values = message
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received an empty Jetson packet"))?
            .splitn(3, ',');

        self.time.append_value(
            values
                .next()
                .ok_or_else(|| anyhow::anyhow!("Received no Jetson timestamp"))?
                .parse()?,
        );
        self.last_current = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received no Jetson current"))?
            .parse()?;
        self.current.append_value(self.last_current);
        self.last_voltage = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Received no Jetson voltage"))?
            .parse()?;
        self.voltage.append_value(self.last_voltage);
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
                Arc::new(self.current.finish()) as ArrayRef,
                Arc::new(self.voltage.finish()) as ArrayRef,
            ],
        )?)
    }

    fn append_final_row(&mut self, elapsed: Duration) {
        self.time.append_value(elapsed.as_micros() as u64);
        self.current.append_value(self.last_current);
        self.voltage.append_value(self.last_voltage);
    }
}

pub(crate) fn get_data_from_jetson(
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
        JetsonData::new(),
        "Jetson",
    )
}
