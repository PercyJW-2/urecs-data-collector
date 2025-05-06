mod network;

use std::str::FromStr;
use clap::{Parser, Subcommand};
use subenum::subenum;

#[derive(Parser, Debug)]
struct Arguments {
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
    println!("{:?}", args);
    match args.command {
        Commands::Single { address, source } => {
            match source {
                Sources::Jetson { data_port, control_port } => {

                },
                Sources::Firmware {} => {

                },
                Sources::FastFirmware { data_port } => {

                }
            }
        },
        Commands::Dual {urecs_address, jetson_address, jetson_data, firmware_type} => {
            match firmware_type {
                Firmware::Firmware {} => {

                },
                Firmware::FastFirmware { data_port } => {

                }
            }
        }
    }
}
