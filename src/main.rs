mod network_firmware;
mod network_firmware_fast;
mod network_jetson;
mod network_shelly_plug;
mod utils;
#[cfg(feature = "visa")]
mod visa_osc_communication;
mod pico_osc_communication;

use std::{fs, fs::File};
use std::fmt::Display;
use anyhow::{anyhow, Result};
use bpaf::Bpaf;
use parse_duration::parse;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{sleep, JoinHandle};
use std::time::Duration;
use parquet::arrow::ArrowWriter;
use subenum::subenum;
use crate::pico_osc_communication::USBInstrumentWrapper;

const IDLE_DURATION: Duration = Duration::from_secs(5);
const PARQUET_BATCH_ROW_COUNT: usize = 1_000_000;

pub(crate) type ShutdownFn = Box<dyn Fn() -> Result<()> + Send + Sync>;

pub(crate) enum DataThreadReturnVal {
    ParquetWriter(ArrowWriter<File>),
    Instrument(USBInstrumentWrapper),
    WriterAndExtraFile((ArrowWriter<File>, PathBuf, String)),
}
pub(crate) type DataThread = JoinHandle<Result<DataThreadReturnVal>>;

#[derive(Debug, Clone)]
pub(crate) enum OscilloscopeMsmtType {
    UCurrent,
    CurrentRanger,
    INA225,
}

impl FromStr for OscilloscopeMsmtType {
    type Err = String;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ucurrent" => Ok(OscilloscopeMsmtType::UCurrent),
            "currentranger" => Ok(OscilloscopeMsmtType::CurrentRanger),
            "ina225" => Ok(OscilloscopeMsmtType::INA225),
            _ => Err(format!("Unknown OscilloscopeMsmtType: {}", s)),
        }
    }
}

impl Display for OscilloscopeMsmtType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UCurrent => write!(f, "UCurrent"),
            Self::CurrentRanger => write!(f, "CurrentRanger"),
            Self::INA225 => write!(f, "INA225"),
        }
    }
}

#[derive(Bpaf, Debug, Clone)]
#[bpaf(options)]
struct Arguments {
    /// All generated csv files are stored at the provided location.
    /// If not provided, the current folder will be used.
    #[bpaf(short, long)]
    storage_path: Option<String>,
    /// Duration, how long to measure
    #[bpaf(short, long, argument::<String>("DURATION"), map(|dur| parse(dur.as_str())))]
    duration: Result<Duration, parse::Error>,
    /// Optional Command that is executed after the measurement begins
    #[bpaf(short, long)]
    command: Option<String>,
    /// First input source to be recorded
    #[bpaf(external, many)]
    sources: Vec<Sources>,
}

#[subenum(Firmware, Jetson, ShellyPlug, Oscilloscope, UsbOscilloscope)]
#[derive(Bpaf, Debug, Clone)]
enum Sources {
    /// Reads data from Jetson using (tegrastats-net)[https://gitlab.ub.uni-bielefeld.de/jwachsmuth/tegrastats-net]
    #[subenum(Jetson)]
    #[bpaf(command, adjacent)]
    Jetson {
        /// Network Address of the Jetson
        #[bpaf(short, long)]
        address: String,
        /// Port on which Data is received
        #[bpaf(short, long)]
        data_port: u16,
        /// Port on which the Data transmission is stopped
        #[bpaf(short, long)]
        control_port: u16,
    },
    /// Reads data from the default u.RECS Firmware
    #[subenum(Firmware)]
    #[bpaf(command, adjacent)]
    Firmware {
        /// Network Address of the u.RECS
        #[bpaf(short, long)]
        address: String,
    },
    /// Reads data from a minimal u.RECS Firmware focussing on fast ADC readouts
    #[subenum(Firmware)]
    #[bpaf(command, adjacent)]
    FastFirmware {
        /// Network Address of the u.RECS
        #[bpaf(short, long)]
        address: String,
        /// Port on which Data is received
        #[bpaf(short, long)]
        data_port: u16,
    },
    /// Reads data from a Shelly PlusPlugS
    #[subenum(ShellyPlug)]
    #[bpaf(command, adjacent)]
    ShellyPlug {
        /// Network Address of the Shelly Plug
        #[bpaf(short, long)]
        address: String,
    },
    /// Reads data from an Oscilloscope
    /// This doesn't work properly
    #[subenum(Oscilloscope)]
    #[bpaf(command, adjacent)]
    Oscilloscope {
    },
    /// Reads data from USB Oscilloscope
    #[subenum(UsbOscilloscope)]
    #[bpaf(command, adjacent)]
    UsbOscilloscope {
        /// Sample-rate that is used, default is 5MS/s
        #[bpaf(short, long, fallback(5000000), display_fallback)]
        sample_rate: u32,
        /// use function-generator of picoscope
        #[bpaf(short, long)]
        use_function_gen: bool,
        /// set measurement type to configure which calibration is used, Options are UCurrent or
        /// CurrentRanger
        #[bpaf(short, long, fallback(OscilloscopeMsmtType::INA225), display_fallback)]
        measurement_type: OscilloscopeMsmtType,
    }
}

fn main() -> Result<()> {
    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()?;

    let args = arguments().run();

    // initialize shutdown function
    let shutdown_funcs = Arc::new(Mutex::new(Vec::<ShutdownFn>::new()));

    log::info!("{args:?}");

    // determine storage folder
    let path = args.storage_path.unwrap_or_else(|| "./".to_string());
    let path = std::path::Path::new(&path);
    if !path.exists() {
        return Err(anyhow!("Path {} does not exist", path.display()));
    }
    if !path.is_dir() {
        return Err(anyhow!("Path {} is not a directory", path.display()));
    }

    // check if defined sources are valid
    let mut jetson_count = 0;
    let mut firmware_count = 0;
    let mut shelly_plug_count = 0;
    let mut oscilloscope_count = 0;
    let mut usb_oscilloscope_count = 0;
    for source in &args.sources {
        if Jetson::try_from(source.clone()).is_ok() {
            jetson_count += 1;
        } else if Firmware::try_from(source.clone()).is_ok() {
            firmware_count += 1;
        } else if ShellyPlug::try_from(source.clone()).is_ok() {
            shelly_plug_count += 1;
        } else if Oscilloscope::try_from(source.clone()).is_ok() {
            oscilloscope_count += 1;
        } else if UsbOscilloscope::try_from(source.clone()).is_ok() {
            usb_oscilloscope_count += 1;
        }
    }
    if jetson_count > 1
        || firmware_count > 1
        || shelly_plug_count > 1
        || oscilloscope_count > 1
        || usb_oscilloscope_count > 1 {
        return Err(anyhow!("The proposed measurement configuration is not possible"));
    }

    // start data acquisition
    // add 10 seconds to runtime to create idle edge at the end and start
    let duration = args.duration?;
    let mut data_threads = Vec::new();
    let read_start = Arc::new(AtomicBool::new(false));
    for source in args.sources {
        match source {
            Sources::Jetson { address, data_port, control_port } => {
                launch_jetson(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    control_port,
                    path.to_path_buf(),
                    read_start.clone(),
                );
            }
            Sources::Firmware { address } => {
                launch_firmware(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    path.to_path_buf(),
                    read_start.clone(),
                );
            }
            Sources::FastFirmware { address, data_port } => {
                launch_fast_firmware(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    path.to_path_buf(),
                    read_start.clone(),
                    duration + (IDLE_DURATION * 2),
                );
            }
            Sources::ShellyPlug { address } => {
                launch_shelly_plug(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    path.to_path_buf(),
                    read_start.clone(),
                )
            }
            Sources::Oscilloscope {} => {
                launch_oscilloscope(
                    &shutdown_funcs,
                    &mut data_threads,
                    path.to_path_buf(),
                    read_start.clone()
                )
            }
            Sources::UsbOscilloscope { sample_rate, use_function_gen, measurement_type } => {
                launch_usb_oscilloscope(
                    &shutdown_funcs,
                    &mut data_threads,
                    path.to_path_buf(),
                    read_start.clone(),
                    sample_rate,
                    use_function_gen,
                    measurement_type,
                )
            }
        }
    }

    log::info!("Starting measurement");
    read_start.store(true, Ordering::Release);

    sleep(IDLE_DURATION);
    let mut command = None;
    if let Some(cmd) = args.command {
        log::info!("Running command: {}", cmd);
        let cmd_split = shell_words::split(&cmd)?;
        command = Some(Command::new(&cmd_split[0])
            .args(&cmd_split[1..])
            .stdout(Stdio::null()) // ignoring output of executed command
            .spawn()?);
    }

    sleep(duration + IDLE_DURATION);

    if let Some(mut cmd) = command {
        log::info!("Waiting for command to finish");
        cmd.wait()?;
    }

    /*let mut buffer = String::new();
    loop {
        io::stdin().read_line(&mut buffer)?;
        if buffer.contains("q") || buffer.contains("stop") {
            break;
        }
    }*/
    log::info!("Shutting down...");
    for func in shutdown_funcs
        .lock()
        .expect("Failed to lock the shutdown hook")
        .iter()
    {
        if let Err(err) = func() {
            log::error!("Error: {err}");
        }
    }

    log::info!("Waiting for threads to stop...");

    for data_thread in data_threads {
        let thread_ret = data_thread.join().expect("DataThread join failed")?;
        log::info!("Flushing Writer");
        match thread_ret {
            DataThreadReturnVal::ParquetWriter(mut wtr) => {
                wtr.flush()?;
                wtr.close()?;
            },
            DataThreadReturnVal::Instrument(instr) => {
                let parquet_handler = Arc::try_unwrap(instr.parquet_handler).expect("Could not unwrap from Arc");
                parquet_handler.flush_and_close()?;
            }
            DataThreadReturnVal::WriterAndExtraFile((mut wtr, path, contents)) => {
                wtr.flush()?;
                wtr.close()?;
                fs::write(path, contents)?;
            }
        }
        log::info!("Writer Flushed");
    }
    Ok(())
}

fn launch_usb_oscilloscope(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
    sample_rate: u32,
    start_func_gen: bool,
    msmt_type: OscilloscopeMsmtType,
) {
    match pico_osc_communication::get_data_from_usb_osc(path, read_start, sample_rate, start_func_gen, msmt_type) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to setup USB Oscilloscope: {error}");
        }
    }
}

#[cfg(feature = "visa")]
fn launch_oscilloscope(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    path_buf: PathBuf,
    read_start: Arc<AtomicBool>,
) {
    match visa_osc_communication::get_data_from_osc(path_buf, read_start) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to set up Oscilloscope: {}", error);
        }
    }
}

#[cfg(not(feature = "visa"))]
fn launch_oscilloscope(
    _shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    _data_threads: &mut Vec<DataThread>,
    _path_buf: PathBuf,
    _read_start: Arc<AtomicBool>,
) {
    log::error!("Visa feature is not compiled, to use this please recompile it with `--features visa`.");
}

fn launch_shelly_plug(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
) {
    match network_shelly_plug::get_data_from_shelly(address, path, read_start) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(err) => {
            log::error!("Failed to set up shelly plug: {err}");
        }
    }
}

fn launch_firmware(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
) {
    match network_firmware::get_data_from_firmware(address, path, read_start) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to set up Firmware networking: {error}");
        }
    }
}

fn launch_fast_firmware(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    port: u16,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
    duration: Duration,
) {
    match network_firmware_fast::get_data_from_fast_firmware(address, port, path, read_start, duration) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to set up Fast firmware networking: {error}");
        }
    }
}

fn launch_jetson(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    jetson_address: String,
    jetson_data_port: u16,
    jetson_control_port: u16,
    path: PathBuf,
    read_start: Arc<AtomicBool>,
) {
    match network_jetson::get_data_from_jetson(
        jetson_address,
        jetson_data_port,
        jetson_control_port,
        path,
        read_start
    ) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to set up Jetson networking: {error}");
        }
    }
}
