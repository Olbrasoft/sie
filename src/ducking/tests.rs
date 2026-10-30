use super::*;
use std::sync::{Arc, Mutex};

struct FakeState {
    sink: Option<Sink>,
    muted: bool,
    writes: Vec<Vec<u32>>,
    reads: usize,
    fail_reads: usize,
    fail_writes: usize,
    write_then_fail: bool,
}

struct Fake {
    state: Arc<Mutex<FakeState>>,
    gate: Option<tokio::sync::oneshot::Receiver<()>>,
}

fn fixture() -> (Controller<Fake>, Arc<Mutex<FakeState>>) {
    let state = Arc::new(Mutex::new(FakeState {
        sink: Some(Sink {
            index: 7,
            name: "test.bose".into(),
            serial: Some("serial-1".into()),
            channels: vec!["front-left".into(), "front-right".into()],
            volumes: vec![50_003, 40_009],
            running: true,
        }),
        muted: true,
        writes: Vec::new(),
        reads: 0,
        fail_reads: 0,
        fail_writes: 0,
        write_then_fail: false,
    }));
    let controller = Controller {
        control: Fake {
            state: state.clone(),
            gate: None,
        },
        sink: "test.bose".into(),
        percent: 20,
        saved: None,
    };
    (controller, state)
}

impl Control for Fake {
    async fn read(&mut self, name: &str) -> Result<Option<Sink>> {
        assert_eq!(name, "test.bose");
        let mut state = self.state.lock().unwrap();
        state.reads += 1;
        if state.fail_reads > 0 {
            state.fail_reads -= 1;
            anyhow::bail!("Injected read failure");
        }
        Ok(state.sink.clone())
    }

    async fn write(&mut self, sink: &Sink, volumes: &[u32]) -> Result<()> {
        if let Some(gate) = self.gate.take() {
            let _ = gate.await;
        }
        let mut state = self.state.lock().unwrap();
        assert!(sink.same_output(state.sink.as_ref().unwrap()));
        if state.fail_writes > 0 {
            state.fail_writes -= 1;
            if state.write_then_fail {
                state.sink.as_mut().unwrap().volumes = volumes.to_vec();
            }
            anyhow::bail!("Injected write failure");
        }
        state.sink.as_mut().unwrap().volumes = volumes.to_vec();
        state.writes.push(volumes.to_vec());
        Ok(())
    }
}

#[tokio::test]
async fn attenuation_preserves_channel_balance_mute_and_exact_base_volume() {
    let (mut controller, state) = fixture();
    controller.update(true).await.unwrap();
    controller.update(false).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.writes, [vec![10_000, 8_001], vec![50_003, 40_009]]);
    assert!(state.muted);
    assert!(controller.saved.is_none());
}

#[tokio::test]
async fn repeated_starts_never_compound_attenuation_and_idle_is_a_noop() {
    let (mut controller, state) = fixture();
    controller.update(false).await.unwrap();
    assert_eq!(state.lock().unwrap().reads, 0);
    for _ in 0..4 {
        controller.update(true).await.unwrap();
    }
    controller.update(false).await.unwrap();
    controller.update(false).await.unwrap();
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn missing_outputs_and_unchanged_volumes_never_receive_writes() {
    for percent in [0, 20, 100] {
        let (mut controller, state) = fixture();
        controller.percent = percent;
        state.lock().unwrap().sink = None;
        controller.update(true).await.unwrap();
        controller.update(false).await.unwrap();
        assert!(state.lock().unwrap().writes.is_empty());
    }
    let (mut controller, state) = fixture();
    controller.percent = 100;
    controller.update(true).await.unwrap();
    controller.update(false).await.unwrap();
    assert!(state.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn manual_adjustments_and_replacement_outputs_are_not_overwritten() {
    for change in 0..4 {
        let (mut controller, state) = fixture();
        controller.update(true).await.unwrap();
        {
            let mut state = state.lock().unwrap();
            match change {
                0 => state.sink.as_mut().unwrap().volumes = vec![20_000, 15_000],
                1 => state.sink.as_mut().unwrap().serial = Some("replacement".into()),
                2 => state.sink.as_mut().unwrap().index = 8,
                _ => state.sink = None,
            }
        }
        controller.update(false).await.unwrap();
        assert_eq!(state.lock().unwrap().writes.len(), 1);
        assert!(controller.saved.is_none());
    }
}

#[tokio::test]
async fn read_or_write_failure_during_restore_retains_the_original_for_retry() {
    for fail_read in [true, false] {
        let (mut controller, state) = fixture();
        controller.update(true).await.unwrap();
        {
            let mut state = state.lock().unwrap();
            if fail_read {
                state.fail_reads = 1;
            } else {
                state.fail_writes = 1;
            }
        }
        assert!(controller.update(false).await.is_err());
        assert!(controller.saved.is_some());
        controller.update(false).await.unwrap();
        assert_eq!(
            state.lock().unwrap().sink.as_ref().unwrap().volumes,
            [50_003, 40_009]
        );
        assert!(controller.saved.is_none());
    }
}

#[tokio::test]
async fn a_failed_attenuation_acknowledgement_can_still_be_restored() {
    let (mut controller, state) = fixture();
    {
        let mut state = state.lock().unwrap();
        state.fail_writes = 1;
        state.write_then_fail = true;
    }
    assert!(controller.update(true).await.is_err());
    controller.update(false).await.unwrap();
    assert_eq!(
        state.lock().unwrap().sink.as_ref().unwrap().volumes,
        [50_003, 40_009]
    );
}

#[tokio::test]
async fn rounding_of_one_volume_unit_does_not_prevent_restoration() {
    let (mut controller, state) = fixture();
    controller.update(true).await.unwrap();
    state.lock().unwrap().sink.as_mut().unwrap().volumes[0] += 1;
    controller.update(false).await.unwrap();
    assert_eq!(
        state.lock().unwrap().writes.last().unwrap(),
        &[50_003, 40_009]
    );
}

async fn wait_for(state: &Arc<Mutex<FakeState>>, condition: impl Fn(&FakeState) -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if condition(&state.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stop_and_shutdown_during_an_inflight_attenuation_restore_serially() {
    let (mut controller, state) = fixture();
    let (release, gate) = tokio::sync::oneshot::channel();
    controller.control.gate = Some(gate);
    let ducking = Ducking::spawn(controller.control, "test.bose".into(), 20);
    ducking.set_recording(true);
    wait_for(&state, |state| state.reads > 0).await;
    // Volume control is still waiting, but capture stop remains nonblocking.
    ducking.set_recording(false);
    release.send(()).unwrap();
    ducking.shutdown().await;
    assert_eq!(
        state.lock().unwrap().writes,
        [vec![10_000, 8_001], vec![50_003, 40_009]]
    );
}

#[tokio::test]
async fn dropping_the_owner_restores_and_disabled_mode_never_starts_a_worker() {
    let (controller, state) = fixture();
    let ducking = Ducking::spawn(controller.control, "test.bose".into(), 20);
    ducking.set_recording(true);
    wait_for(&state, |state| state.writes.len() == 1).await;
    drop(ducking);
    wait_for(&state, |state| state.writes.len() == 2).await;
    let disabled = Ducking::new(None, 20);
    disabled.set_recording(true);
    assert!(disabled.task.is_none());
    disabled.shutdown().await;
}

#[tokio::test]
async fn notification_ownership_waits_for_attenuation_and_restores_after_last_owner() {
    let (controller, state) = fixture();
    let ducking = Ducking::spawn(controller.control, "test.bose".into(), 20);
    let handle = ducking.handle();
    let first = handle.notification().await.unwrap();
    assert_eq!(state.lock().unwrap().writes, [vec![10_000, 8_001]]);
    let second = handle.notification().await.unwrap();
    let recording = handle.recording();
    recording.set_recording(true);
    drop(first);
    drop(second);
    // The acknowledgement for this lease also orders the recording request.
    let last = handle.notification().await.unwrap();
    assert_eq!(state.lock().unwrap().writes.len(), 1);
    drop(last);
    assert!(handle.0.as_ref().unwrap().requested.borrow().recording);
    drop(recording);
    wait_for(&state, |s| s.writes.len() == 2).await;
    ducking.shutdown().await;
    assert_eq!(
        state.lock().unwrap().writes,
        [vec![10_000, 8_001], vec![50_003, 40_009]]
    );
}

#[tokio::test]
async fn notification_skips_idle_output_but_ducks_if_media_starts_during_speech() {
    let (controller, state) = fixture();
    state.lock().unwrap().sink.as_mut().unwrap().running = false;
    let ducking = Ducking::spawn(controller.control, "test.bose".into(), 20);
    let handle = ducking.handle();
    let speech = handle.notification().await.unwrap();
    assert!(state.lock().unwrap().writes.is_empty());
    state.lock().unwrap().sink.as_mut().unwrap().running = true;
    wait_for(&state, |s| s.writes.len() == 1).await;
    drop(speech);
    wait_for(&state, |s| s.writes.len() == 2).await;
    ducking.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_pending_lease_restores_an_inflight_write() {
    let (mut controller, state) = fixture();
    let (release, gate) = tokio::sync::oneshot::channel();
    controller.control.gate = Some(gate);
    let ducking = Ducking::spawn(controller.control, "test.bose".into(), 20);
    let handle = ducking.handle();
    let acquire = tokio::spawn(async move { handle.notification().await });
    wait_for(&state, |s| s.reads > 0).await;
    acquire.abort();
    let _ = acquire.await;
    release.send(()).unwrap();
    wait_for(&state, |s| s.writes.len() == 2).await;
    ducking.shutdown().await;
}

#[tokio::test]
async fn failed_attenuation_is_not_acknowledged_and_can_be_retried() {
    let (mut controller, state) = fixture();
    state.lock().unwrap().fail_writes = 1;
    assert!(controller.update_requested(false, true).await.is_err());
    controller.update_requested(false, true).await.unwrap();
    assert_eq!(state.lock().unwrap().writes, [vec![10_000, 8_001]]);
    controller.update_requested(false, false).await.unwrap();
    assert_eq!(
        state.lock().unwrap().writes.last().unwrap(),
        &[50_003, 40_009]
    );
}

#[test]
fn parser_preserves_channel_map_order_and_commands_use_only_the_snapshot_id() {
    let bytes = br#"[{"index":7,"name":"test.bose","mute":true,
          "channel_map":"front-right,front-left",
          "properties":{"object.serial":"91"},
          "volume":{"front-left":{"value":12345},"front-right":{"value":54321}}}]"#;
    let sink = parse_sink(bytes, "test.bose").unwrap().unwrap();
    assert_eq!(sink.volumes, [54321, 12345]);
    assert_eq!(sink.serial.as_deref(), Some("91"));
    assert_eq!(
        volume_args(&sink, &sink.volumes),
        ["set-sink-volume", "7", "54321", "12345"]
    );
    assert!(parse_sink(bytes, "test.monitor").unwrap().is_none());
    let duplicate = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace("front-right,front-left", "front-left,front-left");
    assert!(parse_sink(duplicate.as_bytes(), "test.bose").is_err());
}
