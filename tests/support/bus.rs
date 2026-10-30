//! Every test owns a private D-Bus daemon; no session bus environment is used.
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};
use zbus::{Connection, connection::Builder};

pub(crate) struct Bus {
    process: Child,
    pub(crate) address: String,
}
impl Bus {
    pub(crate) fn new() -> Self {
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is required for integration tests");
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            process,
            address: address.trim().into(),
        }
    }
    pub(crate) async fn connection(&self) -> Connection {
        Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
