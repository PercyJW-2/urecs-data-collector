use crate::{DataThread, ShutdownFn};
use anyhow::Result;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::SystemTime;
use serde::Serialize;
use visa_rs::enums::status::ErrorCode;
use visa_rs::flags::AccessMode;
use visa_rs::{io_to_vs_err, AsResourceManager, DefaultRM, Error, Instrument, TIMEOUT_IMMEDIATE};

fn setup_osc() -> Result<(Instrument, DefaultRM)> {
    let rm = DefaultRM::new()?;
    let list = rm.find_res_list(&CString::new("?*INSTR")?.into())?;
    let mut possible_targets = Vec::new();
    // Filter valid Targets
    for item in list {
        println!("{:?}", item);
        item.and_then(|item| {
            let item_str = item.to_string_lossy();
            if item_str.starts_with("USB") || item_str.starts_with("ETH") || item_str.starts_with("TCPIP") {
                possible_targets.push(item);
            }
            Ok(())
        })?;
    }
    if possible_targets.len() > 1 {
        todo!("Think about handling more than one possible connection")
    } else if possible_targets.len() < 1 {
        return Err(anyhow::anyhow!("No possible connections"));
    }
    let dev = rm.open(&possible_targets[0], AccessMode::NO_LOCK, TIMEOUT_IMMEDIATE)?;

    Ok((dev, rm))
}

pub(crate) fn get_data_from_osc(path: PathBuf) -> Result<(ShutdownFn, DataThread)> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let mut wtr = csv::Writer::from_path(path.join("osc_data.csv"))?;
    
    let data_thread = thread::spawn(move || -> Result<()> {
        let (mut dev, _rm) = setup_osc()?;
        let mut instrument = InstrumentWrapper::new(&mut dev)?;
        instrument.initialize()?;
        let mut last_time = SystemTime::now();
        instrument.write_inst(b"CURVEStream?\n")?;
        while running.load(std::sync::atomic::Ordering::Relaxed) {
            //let result = instrument.write_read_inst_bin(b"CURve?\n")?;
            let result = instrument.read_bin()?;
            let read_duration = last_time.elapsed()?.as_micros();
            last_time = SystemTime::now();
            println!("Read delay: {}", read_duration/1000);
            result
                .chunks_exact(2)
                .map(|chunk| {
                    u16::from_le_bytes([chunk[0], chunk[1]])
                })
                .enumerate()
                .map(|(i, v)| {
                    OscMeasurement {
                        measurement_time: ((read_duration)/READ_AMOUNT as u128) * i as u128,
                        raw_value: v,
                    }
                })
                .for_each(|measurement| {
                    wtr.serialize(measurement).unwrap();
                });
        }
        instrument.write_inst(b"WFMpre?\n")?;
        println!("Flushing Oscilloscope data writer");
        wtr.flush()?;
        println!("Oscilloscope data writer is finished");
        Ok(())
    });
    
    Ok((
        Box::new(move || {
            println!("Shutting down Osc");
            running_clone.store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }),
        data_thread
    ))
}

const READ_AMOUNT: usize = 250000;

struct InstrumentWrapper<'inst> {
    reader: BufReader<&'inst mut Instrument>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct OscMeasurement {
    measurement_time: u128,
    raw_value: u16
}

impl<'inst> InstrumentWrapper<'inst> {
    fn new(dev: &'inst mut Instrument) -> Result<Self> {
        Ok(Self {
            reader: BufReader::new(dev),
        })
    }
    
    fn initialize(&mut self) -> Result<()> {
        self.dummy_read()?;

        let resp = self.write_read_inst(b"*IDN?\n")?;

        println!("{}", resp);

        // reset instrument to default settings
        //self.write_inst(b"*RST\n")?;

        // initialize instrument TODO: discuss settings commands without * can be seperated by ; symbols
        let setup_commands = [
            b"DATA INIT\n".as_slice(),
            b"DATA SNAP\n".as_slice(),
            b"DATA:START 1;STOP 10000\n",
            b"DATA:ENCDG FAS\n",
            b"DATA:WID 2\n",
            b"DATA:RESO FULL\n",
            b"DATA:SOU CH1\n",
        ];

        //self.write_inst_list(setup_commands.as_slice())?;
        self.write_inst(b"DATA:WID 2\n")?;
        print!("{}", self.write_read_inst(b"DATa?\n")?);
        print!("{}", self.write_read_inst(b"WFMpre?\n")?);
        Ok(())
    }
    
    fn dummy_read(&mut self) -> Result<()> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf).map_err(io_to_vs_err) {
            Ok(_) => {}
            Err(err) => {
                match err {
                    Error(code) => {
                        match code {
                            ErrorCode::ErrorTmo => {
                                println!("Fake reading -> Ignoring Timeout Error");
                            },
                            _ => {
                                return Err(anyhow::format_err!(err));
                            }
                        }
                    }
                };
            }
        };
        Ok(())
    }
    
    fn write_read_inst(&mut self, msg_buf: &[u8]) -> Result<String> {
        self.reader.get_mut().write_all(msg_buf).map_err(io_to_vs_err)?;
        let mut buf = String::new();
        self.reader.read_line(&mut buf).map_err(io_to_vs_err)?;
        Ok(buf)
    }

    fn write_read_inst_bin(&mut self, msg_buf: &[u8]) -> Result<Vec<u8>> {
        self.reader.get_mut().write_all(msg_buf).map_err(io_to_vs_err)?;
        let mut buf = Vec::new();
        self.reader.read_until(b'\n', &mut buf).map_err(io_to_vs_err)?;
        Ok(buf)
    }

    fn read(&mut self) -> Result<String> {
        let mut buf = String::new();
        self.reader.read_line(&mut buf).map_err(io_to_vs_err)?;
        Ok(buf)
    }

    fn read_bin(&mut self) -> Result<[u8; READ_AMOUNT]> {
        let mut buf = [0; READ_AMOUNT];
        self.reader.read_exact(&mut buf).map_err(io_to_vs_err)?;
        Ok(buf)
    }
    
    fn write_inst(&mut self, msg_buf: &[u8]) -> Result<()> {
        match self.reader.get_mut().write_all(msg_buf).map_err(io_to_vs_err) {
            Ok(_) => {},
            Err(e) => {
                match e {
                    Error(code) => {
                        match code {
                            ErrorCode::ErrorTmo => {
                                println!("Timeout writing to instrument");
                            },
                            _ => {
                                return Err(anyhow::anyhow!("Error writing to instrument: {}", e));
                            }
                        }
                    }
                }
            }
        };
        Ok(())
    }
    
    fn write_inst_list(&mut self, msgs: &[&[u8]]) -> Result<()> {
        for msg in msgs {
            print!("{}", String::from_utf8_lossy(msg));
            self.write_inst(msg)?;
        }
        Ok(())
    }
}