//! Synthetic confused-deputy control: filesystem hiding leaves stamping exposed.
use std::{env, fs, io::{self, Read, Write}, net::TcpStream};
fn main() -> io::Result<()> {
    let material = env::var("FIXTURE_MATERIAL").map_err(io::Error::other)?;
    if fs::read(material).is_ok() {
        return Err(io::Error::other("expected hidden fixture material"));
    }
    println!("READ denied");
    let address = env::var("FIXTURE_PROXY").map_err(io::Error::other)?;
    if !address.starts_with("127.0.0.1:") {
        return Err(io::Error::other("fixture only permits loopback"));
    }
    let mut stream = TcpStream::connect(address)?;
    stream.write_all(b"STAMP synthetic-fixture-effect\n")?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    if reply != "ACCEPTED\n" {
        return Err(io::Error::other("fixture proxy refused"));
    }
    println!("USE accepted by vulnerable fake stamping proxy");
    Ok(())
}
