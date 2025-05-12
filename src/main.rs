mod network_jetson;
mod network_firmware;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;
use clap::{Parser, Subcommand};
use subenum::subenum;
use anyhow::Result;

pub(crate) type ShutdownFn = Box<dyn Fn()->Result<()> + Send + Sync>;
pub(crate) type DataThread = thread::JoinHandle<()>;

#[derive(Parser, Debug)]
struct Arguments {
    /// All generated csv files are stored at the provided location.
    /// If not provided, the current folder will be used.
    storage_path: Option<String>,
    #[clap(subcommand)]
    command: Commands,
}

#[subenum(Firmware, Jetson)]
#[derive(Subcommand, Debug, Clone)]
enum Sources {
    /// Reads data from Jetson using (tegrastats-net)[https://gitlab.ub.uni-bielefeld.de/jwachsmuth/tegrastats-net]
    #[subenum(Jetson)]
    Jetson {
        /// Port on which Data is received
        data_port: u16,
        /// Port on which the Data transmission is stopped
        control_port: u16,
    },
    /// Reads data from the default u.RECS Firmware
    #[subenum(Firmware)]
    Firmware {
    },
    /// Reads data from a minimal u.RECS Firmware focussing on fast ADC readouts
    #[subenum(Firmware)]
    FastFirmware {
        /// Port on which Data is received
        data_port: u16,
    },
}

impl FromStr for Jetson {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut splitted = s.split(",");
        let data_port = splitted.next().ok_or("No value provided")?.parse::<u16>().or(Err("Could not parse number"))?;
        let control_port = splitted.next().ok_or("No value provided")?.parse::<u16>().or(Err("Could not parse number"))?;
        let result = Jetson::Jetson { data_port, control_port };
        Ok(result)
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Reads from a Single Source
    Single {
        address: String,
        #[clap(subcommand)]
        source: Sources,
    },
    /// Reads from Jetson and one Firmware-type
    Dual {
        urecs_address: String,
        jetson_address: String,
        /// Jetson Specific data. Provide each value separated by ','
        jetson_data: Jetson,
        #[clap(subcommand)]
        firmware_type: Firmware,
    }
}

fn main() {
    let args = Arguments::parse();

    // initialize shutdown function
    let shutdown_funcs = Arc::new(Mutex::new(Vec::<ShutdownFn>::new()));
    let shutdown_funcs_hook = shutdown_funcs.clone();
    ctrlc::set_handler(move || {
        println!("Shutting down...");
        for func in shutdown_funcs_hook.lock().expect("Failed to lock the shutdown hook").iter() {
            if let Err(err) = func() {
                println!("Error: {}", err);
            }
        }
        println!("Finished shutdown.");
    }).expect("Error setting Ctrl-C handler");

    println!("{:?}", args);

    // determine storage folder
    let path = args.storage_path.unwrap_or_else(|| "./".to_string());
    let path = std::path::Path::new(&path);
    if !path.exists() {
        println!("Path {} does not exist", path.display());
        return;
    }
    if !path.is_dir() {
        println!("Path {} is not a directory", path.display());
        return;
    }

    let mut data_threads = Vec::new();
    match args.command {
        Commands::Single { address, source } => {
            match source {
                Sources::Jetson { data_port, control_port } => {
                    launch_jetson(&shutdown_funcs, &mut data_threads, address, data_port, control_port, path.to_path_buf());
                },
                Sources::Firmware {} => {
                    launch_firmware(&shutdown_funcs, &mut data_threads, address, path.to_path_buf());
                },
                Sources::FastFirmware { data_port } => {

                }
            }
        },
        Commands::Dual {urecs_address, jetson_address, jetson_data, firmware_type} => {
            let jetson_data_port;
            let jetson_control_port;
            match jetson_data {
                Jetson::Jetson { data_port, control_port } => {
                    jetson_data_port = data_port;
                    jetson_control_port = control_port;
                }
            }
            launch_jetson(&shutdown_funcs, &mut data_threads, jetson_address, jetson_data_port, jetson_control_port, path.to_path_buf());
            match firmware_type {
                Firmware::Firmware {} => {
                    launch_firmware(&shutdown_funcs, &mut data_threads, urecs_address, path.to_path_buf());
                },
                Firmware::FastFirmware { data_port } => {

                }
            }
        }
    }
    for data_thread in data_threads {
        data_thread.join().expect("DataThread join failed");
    }
}

fn launch_firmware(shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>, data_threads: &mut Vec<DataThread>, address: String, path: PathBuf) {
    match network_firmware::get_data_from_firmware(address, path) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs.lock().expect("Failed to lock the shutdown hook").push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Firmware networking: {}", error);
        }
    }
}

fn launch_jetson(shutdown_funcs: &Arc<Mutex<Vec<ShutdownFn>>>, data_threads: &mut Vec<DataThread>,
                 jetson_address: String, jetson_data_port: u16, jetson_control_port: u16,
                 path: PathBuf) {
    match network_jetson::get_data_from_jetson(jetson_address, jetson_data_port, jetson_control_port, path) {
        Ok((shutdown_func, data_thread)) => {
            shutdown_funcs.lock().expect("Failed to lock the shutdown hook").push(shutdown_func);
            data_threads.push(data_thread);
        }
        Err(error) => {
            println!("Failed to setup Jetson networking: {}", error);
        }
    }
}
