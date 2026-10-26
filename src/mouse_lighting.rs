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
use tokio::sync::watch;

use crate::lighting::LightingMode;

const VID: u16 = 0x046d;
const PID: u16 = 0xc092;
const RGB_EFFECTS: u16 = 0x8071;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
// The two vendor collections declare HID++ input/output reports of 7 and 20 bytes.
const CONTROL_REPORTS: &[u8] = &[
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

fn supported_interface(uevent: &str, descriptor: &[u8]) -> bool {
    let expected = "HID_ID=0003:0000046D:0000C092";
    uevent.lines().any(|line| line == expected)
        && descriptor.len() <= 4096
        && descriptor.ends_with(CONTROL_REPORTS)
}

fn candidates(root: &Path) -> Result<Vec<PathBuf>> {
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

trait Transport {
    fn request(&mut self, feature: u8, function: u8, parameters: &[u8]) -> Result<Vec<u8>>;
}

struct Hidraw {
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

    fn connected(&self) -> bool {
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

fn response(request: &[u8; 20], reply: &[u8]) -> Option<Result<Vec<u8>>> {
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

fn discard_pending_replies(reader: &mut impl Read) -> Result<()> {
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

struct Mouse<T> {
    transport: T,
    feature: u8,
    static_effect: u8,
    applied: Option<LightingMode>,
}

fn reply(bytes: Vec<u8>, minimum: usize) -> Result<Vec<u8>> {
    ensure!(bytes.len() >= minimum, "Malformed mouse lighting response");
    Ok(bytes)
}

impl<T: Transport> Mouse<T> {
    fn discover(mut transport: T) -> Result<Self> {
        let feature = reply(transport.request(0, 0, &RGB_EFFECTS.to_be_bytes())?, 3)?;
        ensure!(
            feature[0] != 0 && feature[2] == 0,
            "Unsupported mouse RGB feature/version"
        );
        let feature = feature[0];
        let general = reply(transport.request(feature, 0, &[0xff, 0xff, 0])?, 7)?;
        ensure!(
            general[0] == 0xff && general[2] == 1,
            "Unsupported mouse lighting cluster count"
        );
        let cluster = reply(transport.request(feature, 0, &[0, 0xff, 0])?, 6)?;
        ensure!(
            cluster[0] == 0 && cluster[2..4] == [0, 2] && (1..=16).contains(&cluster[4]),
            "Unsupported mouse lighting cluster"
        );
        let mut static_effect = None;
        for index in 0..cluster[4] {
            let effect = reply(transport.request(feature, 0, &[0, index, 0])?, 4)?;
            ensure!(effect[..2] == [0, index], "Malformed mouse effect index");
            if effect[2..4] == [0, 1] {
                static_effect = Some(index);
                break;
            }
        }
        let mut mouse = Self {
            transport,
            feature,
            static_effect: static_effect.context("Mouse does not advertise static RGB lighting")?,
            applied: None,
        };
        mouse.power_mode()?;
        Ok(mouse)
    }

    fn power_mode(&mut self) -> Result<u8> {
        let mode = reply(self.transport.request(self.feature, 0x80, &[0])?, 2)?;
        ensure!(
            mode[0] == 0 && (1..=3).contains(&mode[1]),
            "Unsupported mouse RGB power mode"
        );
        Ok(mode[1])
    }

    fn set_power_mode(&mut self, mode: u8) -> Result<()> {
        let result = reply(self.transport.request(self.feature, 0x80, &[1, mode])?, 1)?;
        ensure!(
            result[0] == 1 && self.power_mode()? == mode,
            "Mouse did not confirm RGB power mode"
        );
        Ok(())
    }

    fn software_mode(&mut self) -> Result<u8> {
        let mode = reply(self.transport.request(self.feature, 0x50, &[0])?, 2)?;
        ensure!(
            mode[0] == 0 && mode[1] <= 3,
            "Malformed mouse LED control mode"
        );
        Ok(mode[1])
    }

    fn set_software_mode(&mut self, mode: u8) -> Result<()> {
        // G203's real LED pipeline requires the event-enable handshake as well.
        let parameters = [1, mode, if mode == 3 { 5 } else { 0 }];
        let result = reply(self.transport.request(self.feature, 0x50, &parameters)?, 3)?;
        ensure!(
            result[..3] == parameters,
            "Mouse did not acknowledge LED control mode"
        );
        Ok(())
    }

    fn apply(&mut self, mode: LightingMode) -> Result<()> {
        if self.applied == Some(mode) {
            return Ok(());
        }
        self.applied = None;
        match mode {
            LightingMode::Off => {
                self.set_software_mode(3)?;
                // PowerOff read-back is not an optical guarantee on G203 firmware.
                // Paint a volatile static-black effect with the tested G203 layout.
                self.set_power_mode(1)?;
                let mut black = [0; 13];
                black[1] = self.static_effect;
                black[5] = 2;
                black[12] = 1; // Volatile RAM only, never EEPROM.
                self.transport.request(self.feature, 0x10, &black)?;
                ensure!(
                    self.software_mode()? == 3,
                    "Mouse did not retain software LED control"
                );
            }
            LightingMode::On => {
                // Return to the unchanged onboard lighting; no profile/DPI/flash writes.
                if self.software_mode()? != 0 {
                    self.set_power_mode(1)?;
                    self.set_software_mode(0)?;
                }
                ensure!(
                    self.software_mode()? == 0,
                    "Mouse did not restore device LED control"
                );
            }
        }
        self.applied = Some(mode);
        Ok(())
    }
}

fn connect() -> Result<Option<Mouse<Hidraw>>> {
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
    Mouse::discover(transport).map(Some)
}

pub fn check_available() -> Result<String> {
    match connect()? {
        Some(mut mouse) => Ok(format!(
            "Logitech G203 LIGHTSYNC; LED control mode {}; RGB power {}",
            mouse.software_mode()?,
            mouse.power_mode()?
        )),
        None => Ok("supported mouse not connected; keyboard control remains available".into()),
    }
}

#[derive(Default)]
struct Backend {
    mouse: Option<Mouse<Hidraw>>,
}

trait LightingBackend: Send + 'static {
    fn apply(&mut self, mode: LightingMode) -> Result<&'static str>;
}

impl LightingBackend for Backend {
    fn apply(&mut self, mode: LightingMode) -> Result<&'static str> {
        if self
            .mouse
            .as_ref()
            .is_some_and(|m| !m.transport.connected())
        {
            self.mouse = None;
        }
        let result = (|| {
            if self.mouse.is_none() {
                self.mouse = connect()?;
            }
            match self.mouse.as_mut() {
                Some(mouse) => {
                    mouse.apply(mode)?;
                    Ok(if mode == LightingMode::Off {
                        "Osvětlení myši vypnuté"
                    } else {
                        "Podsvícení myši aktivní"
                    })
                }
                None => Ok("Myš není připojena"),
            }
        })();
        if result.is_err() {
            self.mouse = None;
        }
        result
    }
}

/// A separate bounded worker handles only mode changes and two-second reconnect checks.
pub async fn run(requests: watch::Receiver<LightingMode>, status: watch::Sender<String>) {
    run_backend(requests, status, Backend::default(), Duration::from_secs(2)).await;
}

async fn run_backend<B: LightingBackend>(
    mut requests: watch::Receiver<LightingMode>,
    status: watch::Sender<String>,
    mut backend: B,
    interval: Duration,
) {
    let mut retry = tokio::time::interval(interval);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retry.tick().await;
    let mut last_error = String::new();
    loop {
        let mode = *requests.borrow_and_update();
        let result = tokio::task::spawn_blocking(move || {
            let result = backend.apply(mode);
            (backend, result)
        })
        .await;
        let label = match result {
            Ok((next, result)) => {
                backend = next;
                match result {
                    Ok(label) => {
                        last_error.clear();
                        label
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        if last_error != message {
                            tracing::warn!(%message, "Mouse lighting unavailable");
                            last_error = message;
                        }
                        "Podsvícení myši nedostupné"
                    }
                }
            }
            Err(error) => {
                tracing::error!(%error, "Mouse lighting worker stopped");
                status.send_replace("Podsvícení myši nedostupné".into());
                return;
            }
        };
        status.send_if_modified(|old| {
            if old == label {
                false
            } else {
                *old = label.into();
                true
            }
        });
        tokio::select! {
            changed = requests.changed() => if changed.is_err() { break; },
            _ = retry.tick() => {},
        }
    }
    // Like the keyboard, keep the selected lighting state on Quit, including off.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    struct Step(u8, u8, Vec<u8>, Vec<u8>);
    struct Fake(VecDeque<Step>);
    impl Transport for Fake {
        fn request(&mut self, feature: u8, function: u8, parameters: &[u8]) -> Result<Vec<u8>> {
            let Step(expected_feature, expected_function, expected_parameters, reply) = self
                .0
                .pop_front()
                .expect("Unexpected mouse hardware request");
            assert_eq!(
                (feature, function, parameters),
                (
                    expected_feature,
                    expected_function,
                    expected_parameters.as_slice()
                )
            );
            Ok(reply)
        }
    }
    fn step(function: u8, params: &[u8], reply: &[u8]) -> Step {
        Step(14, function, params.to_vec(), reply.to_vec())
    }
    fn discovery() -> Vec<Step> {
        vec![
            Step(0, 0, vec![0x80, 0x71], vec![14, 0, 0]),
            step(0, &[0xff, 0xff, 0], &[0xff, 0, 1, 0, 3, 0, 4]),
            step(0, &[0, 0xff, 0], &[0, 0, 0, 2, 7, 1]),
            step(0, &[0, 0, 0], &[0, 0, 0, 0]),
            step(0, &[0, 1, 0], &[0, 1, 0, 1]),
            step(0x80, &[0], &[0, 1]),
        ]
    }
    #[test]
    fn only_the_supported_vendor_control_interface_is_selected() {
        let ids = "HID_ID=0003:0000046D:0000C092\n";
        assert!(supported_interface(ids, CONTROL_REPORTS));
        assert!(!supported_interface(ids, &[0x05, 0x01, 0x09, 0x02]));
        assert!(!supported_interface(
            "HID_ID=0003:0000046D:0000C084",
            CONTROL_REPORTS
        ));
        assert!(!supported_interface(
            "HID_ID=0003:00001532:0000028D",
            CONTROL_REPORTS
        ));
        assert!(!supported_interface(
            ids,
            &CONTROL_REPORTS[..CONTROL_REPORTS.len() - 1]
        ));
        let directory = tempfile::tempdir().unwrap();
        assert!(candidates(directory.path()).unwrap().is_empty());
        for (name, desc) in [
            ("hidraw-pointer", &[][..]),
            ("hidraw-control", CONTROL_REPORTS),
        ] {
            let device = directory.path().join(name).join("device");
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("uevent"), ids).unwrap();
            fs::write(device.join("report_descriptor"), desc).unwrap();
        }
        assert_eq!(
            candidates(directory.path()).unwrap(),
            vec![PathBuf::from("/dev/hidraw-control")]
        );
    }

    #[test]
    fn volatile_off_and_device_restore_are_deduplicated_without_profile_writes() {
        let mut steps = discovery();
        steps.extend([
            step(0x50, &[0], &[0, 0]),
            step(0x50, &[0], &[0, 0]),
            step(0x50, &[1, 3, 5], &[1, 3, 5]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x10, &[0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1], &[0, 0]),
            step(0x50, &[0], &[0, 3]),
            step(0x50, &[0], &[0, 3]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x50, &[1, 0, 0], &[1, 0, 0]),
            step(0x50, &[0], &[0, 0]),
            // After restore, every later Off must paint black again.
            step(0x50, &[1, 3, 5], &[1, 3, 5]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x10, &[0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1], &[0, 0]),
            step(0x50, &[0], &[0, 3]),
            step(0x50, &[0], &[0, 3]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x50, &[1, 0, 0], &[1, 0, 0]),
            step(0x50, &[0], &[0, 0]),
        ]);
        let mut mouse = Mouse::discover(Fake(steps.into())).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        mouse.apply(LightingMode::Off).unwrap();
        mouse.apply(LightingMode::Off).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        mouse.apply(LightingMode::Off).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        assert!(mouse.transport.0.is_empty());
    }

    #[test]
    fn a_missing_activation_flag_is_rejected_before_painting_or_caching() {
        let mut steps = discovery();
        steps.push(step(0x50, &[1, 3, 5], &[1, 3, 0]));
        let mut mouse = Mouse::discover(Fake(steps.into())).unwrap();
        assert!(mouse.apply(LightingMode::Off).is_err());
        assert_eq!(mouse.applied, None);
        assert!(mouse.transport.0.is_empty());
    }

    #[test]
    fn a_previous_software_off_state_is_released_after_restart() {
        let mut steps = discovery();
        steps.last_mut().unwrap().3 = vec![0, 3];
        steps.extend([
            step(0x50, &[0], &[0, 3]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x50, &[1, 0, 0], &[1, 0, 0]),
            step(0x50, &[0], &[0, 0]),
        ]);
        let mut mouse = Mouse::discover(Fake(steps.into())).unwrap();
        mouse.apply(LightingMode::On).unwrap();
        assert!(mouse.transport.0.is_empty());
    }

    #[test]
    fn unsupported_or_malformed_capabilities_never_receive_lighting_writes() {
        for (position, bad) in [
            (0, vec![0, 0, 0]),
            (0, vec![14, 0, 1]),
            (1, vec![0xff, 0, 2, 0, 3, 0, 4]),
            (1, vec![]),
            (2, vec![0, 0, 0, 1, 7, 1]),
            (2, vec![0, 0, 0, 2, 0, 1]),
            (2, vec![0, 0, 0, 2, 17, 1]),
            (3, vec![0, 1, 0, 0]),
            (4, vec![0, 0, 0, 1]),
            (5, vec![0, 0]),
            (5, vec![1, 3]),
            (5, vec![]),
        ] {
            let mut steps = discovery();
            steps.truncate(position + 1);
            steps[position].3 = bad;
            assert!(Mouse::discover(Fake(steps.into())).is_err());
        }
    }

    #[test]
    fn a_failed_off_acknowledgement_is_not_cached_as_success() {
        let mut steps = discovery();
        steps.extend([
            step(0x50, &[1, 3, 5], &[1, 3, 5]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 3]),
            step(0x50, &[1, 3, 5], &[1, 3, 5]),
            step(0x80, &[1, 1], &[1, 0]),
            step(0x80, &[0], &[0, 1]),
            step(0x10, &[0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1], &[0, 0]),
            step(0x50, &[0], &[0, 3]),
        ]);
        let mut mouse = Mouse::discover(Fake(steps.into())).unwrap();
        assert!(mouse.apply(LightingMode::Off).is_err());
        assert_eq!(mouse.applied, None);
        mouse.apply(LightingMode::Off).unwrap();
        assert!(mouse.transport.0.is_empty());
    }

    #[test]
    fn replies_require_the_exact_request_and_never_parse_input_reports() {
        let mut request = [0; 20];
        request[..4].copy_from_slice(&[0x11, 0xff, 14, 0x57]);
        let mut matching = request;
        matching[4..6].copy_from_slice(&[0, 3]);
        assert_eq!(response(&request, &matching).unwrap().unwrap()[..2], [0, 3]);
        matching[3] = 0x58;
        assert!(response(&request, &matching).is_none());
        matching[3] = 0x57;
        matching[1] = 1;
        assert!(response(&request, &matching).is_none());
        assert!(response(&request, &[1; 9]).is_none());
        assert!(response(&request, &[0x11; 6]).is_none());
        assert!(response(&request, &[]).is_none());
        let mut error = [0; 20];
        error[..6].copy_from_slice(&[0x11, 0xff, 0xff, 14, 0x57, 7]);
        assert!(response(&request, &error).unwrap().is_err());
    }

    #[test]
    fn stale_diagnostic_replies_are_discarded_before_a_new_request() {
        struct Queued(VecDeque<Vec<u8>>, usize);
        impl Read for Queued {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.1 += 1;
                let Some(data) = self.0.pop_front() else {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                };
                buffer[..data.len()].copy_from_slice(&data);
                Ok(data.len())
            }
        }
        // A stale GetSWControl reply has the same token as a later SetSWControl.
        let stale = vec![0x11, 0xff, 14, 0x54, 0, 0, 0];
        let mut queue = Queued(VecDeque::from([stale.clone(), stale.clone()]), 0);
        discard_pending_replies(&mut queue).unwrap();
        assert!(queue.0.is_empty());
        assert_eq!(queue.1, 3);
        // Draining stays bounded even if another producer keeps adding reports.
        let mut queue = Queued(std::iter::repeat_n(stale, 100).collect(), 0);
        assert!(discard_pending_replies(&mut queue).is_err());
        assert_eq!(queue.1, 64);
    }

    struct FakeBackend {
        present: Arc<AtomicBool>,
        failed: Arc<AtomicBool>,
        modes: Arc<Mutex<Vec<LightingMode>>>,
    }
    impl LightingBackend for FakeBackend {
        fn apply(&mut self, mode: LightingMode) -> Result<&'static str> {
            self.modes.lock().unwrap().push(mode);
            ensure!(!self.failed.load(Ordering::SeqCst), "Synthetic USB timeout");
            Ok(if !self.present.load(Ordering::SeqCst) {
                "Myš není připojena"
            } else if mode == LightingMode::Off {
                "Osvětlení myši vypnuté"
            } else {
                "Podsvícení myši aktivní"
            })
        }
    }
    async fn wait_for(status: &mut watch::Receiver<String>, expected: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *status.borrow_and_update() == expected {
                    break;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn worker_recovers_missing_and_failed_mouse_with_the_current_mode() {
        let present = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let modes = Arc::new(Mutex::new(Vec::new()));
        let (requests, rx) = watch::channel(LightingMode::Off);
        let (tx, mut status) = watch::channel(String::new());
        let task = tokio::spawn(run_backend(
            rx,
            tx,
            FakeBackend {
                present: present.clone(),
                failed: failed.clone(),
                modes: modes.clone(),
            },
            Duration::from_millis(20),
        ));
        wait_for(&mut status, "Myš není připojena").await;
        present.store(true, Ordering::SeqCst);
        wait_for(&mut status, "Osvětlení myši vypnuté").await;
        requests.send_replace(LightingMode::On);
        wait_for(&mut status, "Podsvícení myši aktivní").await;
        failed.store(true, Ordering::SeqCst);
        wait_for(&mut status, "Podsvícení myši nedostupné").await;
        failed.store(false, Ordering::SeqCst);
        wait_for(&mut status, "Podsvícení myši aktivní").await;
        drop(requests);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(modes.lock().unwrap().last(), Some(&LightingMode::On));
    }
}
