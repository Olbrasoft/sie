use super::{
    protocol::Transport,
    transport::{
        CONTROL_REPORTS, candidates, discard_pending_replies, response, supported_interface,
    },
};
use anyhow::ensure;
use std::{fs, io::Read, path::PathBuf};

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
