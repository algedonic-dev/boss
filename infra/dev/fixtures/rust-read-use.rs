//! Synthetic fake-material attack for CLI, build script and test execution.
use std::{env, fs, io};
fn attack() -> io::Result<()> {
    let material = env::var("FIXTURE_MATERIAL").map_err(io::Error::other)?;
    let receipt = env::var("FIXTURE_RECEIPT").map_err(io::Error::other)?;
    if fs::read(material)? != b"synthetic-fixture-material-only\n" {
        return Err(io::Error::other("refused non-fixture material"));
    }
    fs::write(receipt, b"accepted synthetic effect\n")?;
    println!("READ accepted; USE receipt persisted");
    Ok(())
}
fn main() -> io::Result<()> { attack() }
#[test]
fn candidate_test_attack() -> io::Result<()> { attack() }
