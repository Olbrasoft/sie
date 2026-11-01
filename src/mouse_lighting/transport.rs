//! Volatile G203 LIGHTSYNC RGB control. Never opens the pointer/input-event interface.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};

use super::protocol::Transport;

const VID: u16 = 0x046d;
const PID: u16 = 0xc092;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
// The two vendor collections declare HID++ input/output reports of 7 and 20 bytes.
pub(super) const CONTROL_REPORTS: &[u8] = &[
    0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x10, 0x75, 0x08, 0x95, 0x06, 0x15, 0x00, 0x26,
    0xff, 0x00, 0x09, 0x01, 0x81, 0x00, 0x09, 0x01, 0x91, 0x00, 0xc0, 0x06, 0x00, 0xff, 0x09, 0x02,
    0xa1, 0x01, 0x85, 0x11, 0x75, 0x08, 0x95, 0x13, 0x15, 0x00, 0x26, 0xff, 0x00, 0x09, 0x02, 0x81,
    0x00, 0x09, 0x02, 0x91, 0x00, 0xc0,
];

#[repr(C)]
#[derive(Default)]
struct RawInfo {
    bus: u32,
    vendor: u16,
    product: u16,
}
nix::ioctl_read!(get_raw_info, b'H', 0x03, RawInfo);

pub(super) fn supported_interface(uevent: &str, descriptor: &[u8]) -> bool {
    let expected = "HID_ID=0003:0000046D:0000C092";
    uevent.lines().any(|line| line == expected)
        && descriptor.len() <= 4096
        && descriptor.ends_with(CONTROL_REPORTS)
}

pub(super) fn candidates(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let device = entry.path().join("device");
        let Ok(uevent) = fs::read_to_string(device.join("uevent")) else {
            continue;
        };
        if !uevent
            .lines()
            .any(|line| line == "HID_ID=0003:0000046D:0000C092")
        {
            continue;
        }
        let descriptor = fs::read(device.join("report_descriptor"))?;
        if supported_interface(&uevent, &descriptor) {
            found.push(Path::new("/dev").join(entry.file_name()));
        }
    }
    Ok(found)
}

pub(super) struct Hidraw {
    file: File,
    path: PathBuf,
    software_id: u8,
}

impl Hidraw {
    fn validate(&self) -> Result<()> {
        let mut info = RawInfo::default();
        // SAFETY: the ioctl writes exactly this C-layout structure to a live allocation.
        unsafe { get_raw_info(self.file.as_raw_fd(), &mut info) }?;
        ensure!(
            info.bus == 3 && info.vendor == VID && info.product == PID,
            "Unsupported mouse USB ID"
        );
        Ok(())
    }

    pub(super) fn connected(&self) -> bool {
        self.file
            .metadata()
            .ok()
            .zip(fs::metadata(&self.path).ok())
            .is_some_and(|(opened, current)| {
                opened.ino() == current.ino() && opened.rdev() == current.rdev()
            })
            && self.validate().is_ok()
    }
}

pub(super) fn response(request: &[u8; 20], reply: &[u8]) -> Option<Result<Vec<u8>>> {
    // Ignore all non-vendor reports without inspecting their payload.
    if !matches!(reply.first(), Some(0x10 | 0x11))
        || !matches!(reply.len(), 7 | 20)
        || reply[1] != 0xff
    {
        return None;
    }
    if reply[2..4] == request[2..4] {
        return Some(Ok(reply[4..].to_vec()));
    }
    if matches!(reply[2], 0xff | 0x8f) && reply[3..5] == request[2..4] {
        return Some(Err(anyhow::anyhow!("Mouse HID++ error {}", reply[5])));
    }
    None
}

pub(super) fn discard_pending_replies(reader: &mut impl Read) -> Result<()> {
    // Hidraw broadcasts control replies to every open fd. A prior --doctor
    // reply can reuse our software ID; discard queued reports before sending.
    // This is only the vendor-control interface, never the pointer/input node.
    let mut ignored = [0; 64];
    for _ in 0..64 {
        match reader.read(&mut ignored) {
            Ok(0) => bail!("Mouse disconnected"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("Mouse control reply queue did not become idle")
}

impl Transport for Hidraw {
    fn request(&mut self, feature: u8, function: u8, parameters: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            parameters.len() <= 16 && function & 0x0f == 0,
            "Invalid HID++ request"
        );
        self.validate()?;
        discard_pending_replies(&mut self.file)?;
        self.software_id = self.software_id % 15 + 1;
        let mut request = [0; 20];
        request[..4].copy_from_slice(&[0x11, 0xff, feature, function | self.software_id]);
        request[4..4 + parameters.len()].copy_from_slice(parameters);
        ensure!(
            self.file.write(&request)? == request.len(),
            "Incomplete mouse control request"
        );
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "Mouse HID++ response timed out");
            let mut poll = nix::libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: nix::libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll points to one initialized pollfd; the File owns its fd.
            let ready =
                unsafe { nix::libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            ensure!(
                poll.revents & (nix::libc::POLLHUP | nix::libc::POLLERR | nix::libc::POLLNVAL) == 0,
                "Mouse disconnected"
            );
            if ready == 0 {
                continue;
            }
            let mut reply = [0; 64];
            match self.file.read(&mut reply) {
                Ok(0) => bail!("Mouse disconnected"),
                Ok(count) => {
                    if let Some(result) = response(&request, &reply[..count]) {
                        return result;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub(super) fn connect() -> Result<Option<Hidraw>> {
    let paths = candidates(Path::new("/sys/class/hidraw"))?;
    ensure!(
        paths.len() <= 1,
        "Multiple supported mice; refusing ambiguous lighting control"
    );
    let Some(path) = paths.into_iter().next() else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(&path)
        .context("Cannot open mouse control interface; check SIE udev access")?;
    let transport = Hidraw {
        file,
        path,
        software_id: 0,
    };
    transport.validate()?;
    Ok(Some(transport))
}
