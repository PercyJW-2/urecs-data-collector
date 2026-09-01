mod network_firmware;
mod network_firmware_fast;
mod network_jetson;
mod network_shelly_plug;
mod utils;
mod pico_osc_communication;
mod tekhsi_osc_communication;
mod network_hailo_rt;

use std::{fs, fs::File};
use std::fmt::Display;
use anyhow::{anyhow, Result};
use bpaf::Bpaf;
use parse_duration::parse;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Barrier, Mutex};
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

#[derive(Debug, Clone)]
pub(crate) enum MsmtEnvironment {
    Jetson,
    M2,
    TriggerChannel,
}

impl FromStr for MsmtEnvironment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "jetson" => Ok(Self::Jetson),
            "m.2" => Ok(Self::M2),
            "triggerchannel" => Ok(Self::TriggerChannel),
            _ => Err(format!("Unknown MsmtEnvironment: {}", s)),
        }
    }
}

impl Display for MsmtEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jetson => write!(f, "Jetson"),
            Self::M2 => write!(f, "M.2"),
            Self::TriggerChannel => write!(f, "TriggerChannel"),
        }
    }
}

#[derive(Debug, Clone)]
enum OscilloscopeProbeFactor {
    X1,
    X10,
}

impl FromStr for OscilloscopeProbeFactor {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "x1" => Ok(OscilloscopeProbeFactor::X1),
            "x10" => Ok(OscilloscopeProbeFactor::X10),
            _ => Err(format!("Unknown OscilloscopeProbeFactor: {}", s)),
        }
    }
}

impl Into<f64> for OscilloscopeProbeFactor {
    fn into(self) -> f64 {
        match self {
            Self::X1 => 1.0,
            Self::X10 => 10.0,
        }
    }
}

impl Display for OscilloscopeProbeFactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OscilloscopeProbeFactor::X1 => write!(f, "X1"),
            OscilloscopeProbeFactor::X10 => write!(f, "X10"),
        }
    }
}

#[derive(Clone, Debug)]
enum BenchmarkCommand {
    TimedEngineExecution{
        engine_path: String,
    },
    JetsonCommand(String),
    OtherCommand(String),
    NoCommand,
}

impl FromStr for BenchmarkCommand {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("TEE") {
            let mut split = s.split(",");
            split.next().ok_or("Impossible State")?;
            Ok(BenchmarkCommand::TimedEngineExecution {
                engine_path: split.next().ok_or("no path")?.to_string(),
            })
        } else if s.starts_with("JET") {
            let (_, split) = s.split_once(" ").ok_or("No Command")?;
            Ok(BenchmarkCommand::JetsonCommand(split.trim().to_string()))
        } else {
            Ok(BenchmarkCommand::OtherCommand(s.to_string()))
        }
    }
}

impl Display for BenchmarkCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BenchmarkCommand::TimedEngineExecution { engine_path } =>
                write!(f, "TimedEngineExecution({})", engine_path),
            BenchmarkCommand::JetsonCommand(command) =>
                write!(f, "JetsonCommand({})", command),
            BenchmarkCommand::OtherCommand(command) =>
                write!(f, "OtherCommand({})", command),
            BenchmarkCommand::NoCommand =>
                write!(f, "NoCommand"),
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
    /// Duration, how long to measure. Take a look at the (parse_duration)[https://docs.rs/parse_duration/latest/parse_duration/] crate for formatting details
    #[bpaf(short, long, argument::<String>("DURATION"), map(|dur| parse(dur.as_str())))]
    duration: Result<Duration, parse::Error>,
    /// Duration, how long to measure before the actual measurement. Uses parse_duration crate formatting. Default is 5 Seconds
    #[bpaf(short('b'), long, argument::<String>("DURATION"), map(|dur| parse(dur.as_str())), fallback(Ok(IDLE_DURATION)))]
    pre_duration: Result<Duration, parse::Error>,
    /// Duration, how long to measure after the actual measurement. Uses parse_duration crate formatting. Default is 5 Seconds
    #[bpaf(short('e'), long, argument::<String>("DURATION"), map(|dur| parse(dur.as_str())), fallback(Ok(IDLE_DURATION)))]
    post_duration: Result<Duration, parse::Error>,
    /// Optional Command that is executed after the measurement begins
    #[bpaf(short, long, fallback(BenchmarkCommand::NoCommand))]
    command: BenchmarkCommand,
    /// First input source to be recorded
    #[bpaf(external, many)]
    sources: Vec<Sources>,
}

#[subenum(Firmware, Jetson, ShellyPlug, Oscilloscope, UsbOscilloscope, HailoRT)]
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
    /// Reads data from Hailo Accelerator using (hailort-msmt)[https://github.com/PercyJW-2/hailort-msmt]
    #[subenum(HailoRT)]
    #[bpaf(command, adjacent)]
    HailoRT {
        /// Network Address of HailoRT Host
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
        /// Channel on which the ADC is reading - Default is 2 (Jetson Current)
        #[bpaf(short, long, fallback(2), display_fallback)]
        channel: u8,
        /// Sample-rate that is used, default is 2kS/s
        #[bpaf(short, long, fallback(2000), display_fallback)]
        sample_rate: u16,
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
    /// visa feature is needed to control settings
    /// This measurement starts instantly and the provided duration is directly used
    #[subenum(Oscilloscope)]
    #[bpaf(command, adjacent)]
    Oscilloscope {
        /// Network Address of the Tektronix Oscilloscope
        #[bpaf(short, long)]
        address: String,
        /// Sample-rate that is used, default is 5MS/s
        #[bpaf(short, long, fallback(5000000), display_fallback)]
        sample_rate: u32,
        /// Frame duration, Often times this method has trouble to capture the complete measurement
        /// thus this separate duration is introduced
        #[bpaf(short, long, argument::<String>("DURATION"), map(|dur| parse(dur.as_str()).unwrap()), fallback(IDLE_DURATION))]
        duration: Duration,
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
        /// set measured object to configure correct probe settings
        #[bpaf(short, long, fallback(MsmtEnvironment::Jetson), display_fallback)]
        msmt_environment: MsmtEnvironment,
        /// Selects the probe factor used, its either X1 or X10, while the default is X10
        #[bpaf(short, long, fallback(OscilloscopeProbeFactor::X10), display_fallback)]
        current_channel_probe_factor: OscilloscopeProbeFactor,
        /// Selects the probe factor used, its either X1 or X10, while the default is X10
        #[bpaf(short, long, fallback(OscilloscopeProbeFactor::X10), display_fallback)]
        voltage_channel_probe_factor: OscilloscopeProbeFactor,
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
    let mut hailo_rt_count = 0;
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
        } else if HailoRT::try_from(source.clone()).is_ok() {
            hailo_rt_count += 1;
        }
    }
    if jetson_count > 1
        || firmware_count > 1
        || shelly_plug_count > 1
        || oscilloscope_count > 1
        || usb_oscilloscope_count > 1
        || hailo_rt_count > 1 {
        return Err(anyhow!("The proposed measurement configuration is currently not possible"));
    }

    // start data acquisition
    // add 10 seconds to runtime to create idle edge at the end and start
    let duration = args.duration?;
    let pre_duration = args.pre_duration?;
    let post_duration = args.post_duration?;
    let mut data_threads = Vec::new();
    let read_start = Arc::new(Barrier::new(args.sources.len() + 1));
    let mut osc_duration = None;
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
            Sources::HailoRT { address, data_port, control_port } => {
                launch_hailo_rt(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    control_port,
                    path.to_path_buf(),
                    read_start.clone(),
                )
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
            Sources::FastFirmware { address, data_port, channel , sample_rate} => {
                launch_fast_firmware(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    path.to_path_buf(),
                    read_start.clone(),
                    channel,
                    duration + (IDLE_DURATION * 2),
                    sample_rate,
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
            Sources::Oscilloscope { address, sample_rate, duration } => {
                osc_duration = Some(format!("{}", duration.as_secs() + 1));
                launch_oscilloscope(
                    address,
                    sample_rate,
                    duration,
                    &shutdown_funcs,
                    &mut data_threads,
                    path.to_path_buf(),
                    read_start.clone()
                )
            }
            Sources::UsbOscilloscope {
                sample_rate,
                use_function_gen,
                measurement_type,
                msmt_environment,
                current_channel_probe_factor,
                voltage_channel_probe_factor,
            } => {
                launch_usb_oscilloscope(
                    &shutdown_funcs,
                    &mut data_threads,
                    path.to_path_buf(),
                    read_start.clone(),
                    sample_rate,
                    use_function_gen,
                    measurement_type,
                    msmt_environment,
                    current_channel_probe_factor,
                    voltage_channel_probe_factor,
                )
            }
        }
    }

    log::info!("Starting measurement");
    read_start.wait();

    sleep(pre_duration);
    let mut command = None;
    if let BenchmarkCommand::NoCommand = args.command {
    } else {
        let cmd = match args.command {
            BenchmarkCommand::TimedEngineExecution { engine_path } => {
                let trigger_wait = osc_duration.unwrap_or("500".to_string());
                format!(
                    "ssh nx@10.42.0.44 ~/timed_engine_execution.sh {engine_path} {} {trigger_wait}",
                    duration.as_secs()
                )
            },
            BenchmarkCommand::JetsonCommand(cmd) => format!("ssh nx@10.42.0.44 {cmd}"),
            BenchmarkCommand::OtherCommand(cmd) => cmd,
            BenchmarkCommand::NoCommand => {"".to_string()}
        };
        log::info!("Running command: {}", cmd);
        let cmd_split = shell_words::split(&cmd)?;
        command = Some(Command::new(&cmd_split[0])
            .args(&cmd_split[1..])
            .stdout(Stdio::null())
            .spawn()?
        );
    }

    sleep(duration + post_duration);

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
    read_start: Arc<Barrier>,
    sample_rate: u32,
    start_func_gen: bool,
    msmt_type: OscilloscopeMsmtType,
    msmt_environment: MsmtEnvironment,
    current_channel_probe_factor: OscilloscopeProbeFactor,
    voltage_channel_probe_factor: OscilloscopeProbeFactor,
) {
    match pico_osc_communication::get_data_from_usb_osc(
        path,
        read_start,
        sample_rate,
        start_func_gen,
        msmt_type,
        msmt_environment,
        current_channel_probe_factor,
        voltage_channel_probe_factor
    ) {
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

fn launch_oscilloscope(
    address: String,
    sample_rate: u32,
    duration: Duration,
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    path_buf: PathBuf,
    read_start: Arc<Barrier>,
) {
    match tekhsi_osc_communication::get_data_from_tek_hsi_oscilloscope(
        address,
        sample_rate,
        duration,
        read_start,
        path_buf
    ) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            log::error!("Failed to setup TekHSI Communication: {error}");
        }
    }
}

fn launch_shelly_plug(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    path: PathBuf,
    read_start: Arc<Barrier>,
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
    read_start: Arc<Barrier>,
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
    read_start: Arc<Barrier>,
    channel: u8,
    duration: Duration,
    sample_rate: u16,
) {
    match network_firmware_fast::get_data_from_fast_firmware(address, port, path, read_start, channel, duration, sample_rate) {
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
    read_start: Arc<Barrier>,
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

fn launch_hailo_rt(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    hailo_rt_address: String,
    hailo_rt_data_port: u16,
    hailo_rt_control_port: u16,
    path: PathBuf,
    read_start: Arc<Barrier>,
) {
    match network_hailo_rt::get_data_from_hailo_rt(
        hailo_rt_address,
        hailo_rt_data_port,
        hailo_rt_control_port,
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
            log::error!("Failed to set up HiloRt networking: {error}");
        }
    }
}
