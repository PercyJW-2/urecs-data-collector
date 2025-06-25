mod network_firmware;
mod network_firmware_fast;
mod network_jetson;
mod network_shelly_plug;
mod utils;
mod visa_osc_communication;

use anyhow::{anyhow, Result};
use bpaf::Bpaf;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use subenum::subenum;

pub(crate) type ShutdownFn = Box<dyn Fn() -> Result<()> + Send + Sync>;
pub(crate) type DataThread = JoinHandle<Result<()>>;

#[derive(Bpaf, Debug, Clone)]
#[bpaf(options)]
struct Arguments {
    /// All generated csv files are stored at the provided location.
    /// If not provided, the current folder will be used.
    #[bpaf(short, long)]
    storage_path: Option<String>,
    /// First input source to be recorded
    #[bpaf(external, many)]
    sources: Vec<Sources>,
}

#[subenum(Firmware, Jetson, ShellyPlug, Oscilloscope)]
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
    #[subenum(Oscilloscope)]
    #[bpaf(command, adjacent)]
    Oscilloscope {
        
    }
}

fn main() -> Result<()> {
    let args = arguments().run();

    // initialize shutdown function
    let shutdown_funcs = Arc::new(Mutex::new(Vec::<ShutdownFn>::new()));
    let shutdown_funcs_hook = shutdown_funcs.clone();
    ctrlc::set_handler(move || {
        println!("Shutting down...");
        for func in shutdown_funcs_hook
            .lock()
            .expect("Failed to lock the shutdown hook")
            .iter()
        {
            if let Err(err) = func() {
                println!("Error: {}", err);
            }
        }
        println!("Finished shutdown.");
    })
    .expect("Error setting Ctrl-C handler");

    println!("{:?}", args);

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
    for source in &args.sources {
        if let Ok(_) = Jetson::try_from(source.clone()) {
            jetson_count += 1;
        } else if let Ok(_) = Firmware::try_from(source.clone()) {
            firmware_count += 1;
        } else if let Ok(_) = ShellyPlug::try_from(source.clone()) {
            shelly_plug_count += 1;
        } else if let Ok(_) = Oscilloscope::try_from(source.clone()) {
            oscilloscope_count += 1;
        }
    }
    if jetson_count > 1 || firmware_count > 1 || shelly_plug_count > 1 || oscilloscope_count > 1 {
        return Err(anyhow!("The proposed measurement configuration is not possible"));
    }

    // start data acquisition
    let mut data_threads = Vec::new();
    for source in args.sources {
        match source {
            Sources::Jetson { address, data_port, control_port } => {
                launch_jetson(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    control_port,
                    path.to_path_buf()
                );
            }
            Sources::Firmware { address } => {
                launch_firmware(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    path.to_path_buf()
                );
            }
            Sources::FastFirmware { address, data_port } => {
                launch_fast_firmware(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    data_port,
                    path.to_path_buf()
                );
            }
            Sources::ShellyPlug { address } => {
                launch_shelly_plug(
                    &shutdown_funcs,
                    &mut data_threads,
                    address,
                    path.to_path_buf()
                )
            }
            Sources::Oscilloscope {} => {
                //TODO this is very temporay
                launch_oscilloscope(
                    &shutdown_funcs,
                    &mut data_threads,
                )
            }
        }
    }

    for data_thread in data_threads {
        data_thread.join().expect("DataThread join failed")?;
    }
    Ok(())
}

fn launch_oscilloscope(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
) {
    match visa_osc_communication::get_data_from_osc() {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Oscilloscope networking: {}", error);
        }
    }
}

fn launch_shelly_plug(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    path: PathBuf
) {
    match network_shelly_plug::get_data_from_shelly(address, path) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(err) => {
            println!("Failed to setup shelly plug: {}", err);
        }
    }
}

fn launch_firmware(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    path: PathBuf,
) {
    match network_firmware::get_data_from_firmware(address, path) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Firmware networking: {}", error);
        }
    }
}

fn launch_fast_firmware(
    shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>,
    data_threads: &mut Vec<DataThread>,
    address: String,
    port: u16,
    path: PathBuf,
) {
    match network_firmware_fast::get_data_from_fast_firmware(address, port, path) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Fast firmware networking: {}", error);
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
) {
    match network_jetson::get_data_from_jetson(
        jetson_address,
        jetson_data_port,
        jetson_control_port,
        path,
    ) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs
                .lock()
                .expect("Failed to lock the shutdown hook")
                .push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Jetson networking: {}", error);
        }
    }
}
