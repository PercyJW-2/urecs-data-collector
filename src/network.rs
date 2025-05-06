use std::io;
use std::io::Error;
use std::net::UdpSocket;

fn get_data_from_jetson(address: String, data_port: u16, control_port: u16) -> Result<(), Error> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", data_port))?;
    socket.connect(format!("{}:{}", address, data_port))?;
    Ok(())
}