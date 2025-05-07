use std::io::{Write};
use std::net::{TcpStream, UdpSocket};
use std::thread;
use std::time::Duration;
use http_req::request::{Authentication, Method, Request};
use http_req::uri::Uri;
use serde::Deserialize;
use anyhow::Result;

pub(crate) type ShutdownFn = Box<dyn Fn()->Result<()> + Send + Sync>;
pub(crate) type DataThread = thread::JoinHandle<()>;

pub(crate) fn get_data_from_jetson(address: String, data_port: u16, control_port: u16) -> Result<(ShutdownFn, DataThread)> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", data_port))?;
    socket.connect(format!("{}:{}", address, data_port))?;
    let data_thread = thread::spawn(move || {

    });
    Ok((
        Box::new(move || {
            println!("Shutting down Jetson Interface");
            let mut control_connection = TcpStream::connect(format!("{}:{}", address, control_port))?;
            control_connection.write("stop\n".as_bytes())?;
            Ok(())
        }),
        data_thread
    ))
}

#[derive(Debug, Deserialize)]
struct NodeJetson {
    #[serde(rename = "@maxPowerUsage")]
    max_power_usage: f64,
    #[serde(rename = "@architecture")]
    architecture: String,
    #[serde(rename = "@baseboardId")]
    baseboard_id: String,
    #[serde(rename = "@voltage")]
    voltage: f64,
    #[serde(rename = "@actualNodePowerUsage")]
    actual_node_power_usage: f64,
    #[serde(rename = "@actualPowerUsage")]
    actual_power_usage: f64,
    #[serde(rename = "@state")]
    state: u8,
    #[serde(rename = "@lastPowerState")]
    last_power_state: u8,
    #[serde(rename = "@defaultPowerState")]
    default_power_state: u8,
    #[serde(rename = "@rcuId")]
    rcu_id: String,
    #[serde(rename = "@health")]
    health: String,
    #[serde(rename = "@lastSensorUpdate")]
    last_sensor_update: usize,
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@present")]
    present: bool,
    #[serde(rename = "@forceRecovery")]
    force_recovery: bool,
    #[serde(rename = "@jetsonType")]
    jetson_type: String,
    #[serde(rename = "@baseboardPosition")]
    baseboard_position: u8
}

pub(crate) fn get_data_from_firmware(address: String) -> Result<(ShutdownFn, DataThread)> {
    // this is hideous, but I don't know a different option
    let uri_str: &'static str = format!("https://{}/REST/node/RCU_0_BB_1_1", address).leak();
    let uri = Uri::try_from(uri_str)?;
    let data_thread = thread::spawn(move || {
        let mut last_voltage:f64 = 0.0;
        let mut last_power:f64 = 0.0;
        loop {
            let mut body = Vec::new();
            let _ = Request::new(&uri)
                .authentication(Authentication::basic("admin", "admin"))
                .method(Method::GET)
                .send(&mut body);
            let body = String::from_utf8_lossy(&body);
            let node: NodeJetson = quick_xml::de::from_str(&body).expect("Could not parse node XML");
            if last_voltage != node.voltage || last_power != node.actual_power_usage { 
                last_voltage = node.voltage;
                last_power = node.actual_power_usage;
                println!("Measurement: {}V, {}mW", node.voltage, node.actual_power_usage);
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
    Ok((
        Box::new(move || {
            println!("Shutting down Firmware Interface");
            Ok(())
        }),
        data_thread
    ))
}