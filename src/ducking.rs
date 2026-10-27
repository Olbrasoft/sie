//! Recording-only attenuation of one explicit output, independent of inference.
use std::{future::Future, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tokio::{process::Command, sync::watch, task::JoinHandle};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sink {
    index: u32,
    name: String,
    serial: Option<String>,
    channels: Vec<String>,
    volumes: Vec<u32>,
}

impl Sink {
    fn same_output(&self, other: &Self) -> bool {
        self.index == other.index
            && self.name == other.name
            && self.serial == other.serial
            && self.channels == other.channels
    }
}

trait Control: Send {
    fn read(&mut self, name: &str) -> impl Future<Output = Result<Option<Sink>>> + Send;
    fn write(&mut self, sink: &Sink, volumes: &[u32]) -> impl Future<Output = Result<()>> + Send;
}

struct Pactl;

fn parse_sink(bytes: &[u8], name: &str) -> Result<Option<Sink>> {
    #[derive(Deserialize)]
    struct Volume {
        value: u32,
    }
    #[derive(Deserialize)]
    struct RawSink {
        index: u32,
        name: String,
        channel_map: String,
        volume: std::collections::HashMap<String, Volume>,
        #[serde(default)]
        properties: serde_json::Map<String, serde_json::Value>,
    }
    let sinks: Vec<RawSink> =
        serde_json::from_slice(bytes).context("Invalid audio output state")?;
    let Some(raw) = sinks.into_iter().find(|sink| sink.name == name) else {
        return Ok(None);
    };
    let channels: Vec<String> = raw
        .channel_map
        .split(',')
        .map(|channel| channel.trim().to_owned())
        .collect();
    ensure!(
        !channels.is_empty()
            && channels.len() <= 32
            && channels.iter().all(|channel| !channel.is_empty())
            && channels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == channels.len()
            && channels.len() == raw.volume.len(),
        "Invalid audio channel map"
    );
    let volumes = channels
        .iter()
        .map(|channel| {
            raw.volume
                .get(channel)
                .map(|volume| volume.value)
                .context("Missing audio channel volume")
        })
        .collect::<Result<Vec<_>>>()?;
    let serial = raw.properties.get("object.serial").map(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    });
    Ok(Some(Sink {
        index: raw.index,
        name: raw.name,
        serial,
        channels,
        volumes,
    }))
}

async fn pactl(args: &[String]) -> Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_millis(500),
        Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Audio volume control timed out")??;
    ensure!(output.status.success(), "Audio volume control failed");
    Ok(output.stdout)
}

fn volume_args(sink: &Sink, volumes: &[u32]) -> Vec<String> {
    let mut args = vec!["set-sink-volume".into(), sink.index.to_string()];
    args.extend(volumes.iter().map(u32::to_string));
    args
}

impl Control for Pactl {
    async fn read(&mut self, name: &str) -> Result<Option<Sink>> {
        let bytes = pactl(&["-f".into(), "json".into(), "list".into(), "sinks".into()]).await?;
        parse_sink(&bytes, name)
    }

    async fn write(&mut self, sink: &Sink, volumes: &[u32]) -> Result<()> {
        pactl(&volume_args(sink, volumes)).await?;
        Ok(())
    }
}

struct Saved {
    original: Sink,
    applied: Vec<u32>,
}

struct Controller<C> {
    control: C,
    sink: String,
    percent: u8,
    saved: Option<Saved>,
}

fn same_volume(left: &[u32], right: &[u32]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.abs_diff(*b) <= 1)
}

impl<C: Control> Controller<C> {
    async fn update(&mut self, recording: bool) -> Result<()> {
        if recording {
            if self.saved.is_some() {
                return Ok(());
            }
            let Some(original) = self.control.read(&self.sink).await? else {
                return Ok(());
            };
            let applied: Vec<_> = original
                .volumes
                .iter()
                .map(|value| (u64::from(*value) * u64::from(self.percent) / 100) as u32)
                .collect();
            if applied == original.volumes {
                return Ok(());
            }
            // Save before the write: a failed acknowledgement may still have
            // changed the device, so cleanup must retain the original state.
            self.saved = Some(Saved { original, applied });
            let saved = self.saved.as_ref().unwrap();
            self.control.write(&saved.original, &saved.applied).await?;
            tracing::info!(output = %self.sink, "Media output attenuated for recording");
        } else if let Some(saved) = &self.saved {
            let current = self.control.read(&self.sink).await?;
            if let Some(current) = current
                && saved.original.same_output(&current)
                && same_volume(&current.volumes, &saved.applied)
            {
                self.control
                    .write(&current, &saved.original.volumes)
                    .await?;
                tracing::info!(output = %self.sink, "Media output volume restored");
            }
            // A replacement device or a manual adjustment supersedes our lease.
            self.saved = None;
        }
        Ok(())
    }
}

pub(crate) struct Ducking {
    requested: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
}

impl Ducking {
    pub fn new(sink: Option<String>, percent: u8) -> Self {
        match sink {
            Some(sink) => Self::spawn(Pactl, sink, percent),
            None => Self {
                requested: None,
                task: None,
            },
        }
    }

    fn spawn(control: impl Control + 'static, sink: String, percent: u8) -> Self {
        let (requested, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut controller = Controller {
                control,
                sink,
                percent,
                saved: None,
            };
            loop {
                tokio::select! {
                    changed = receiver.changed() => if changed.is_err() { break; },
                    _ = tokio::time::sleep(Duration::from_millis(250)),
                        if controller.saved.is_some() && !*receiver.borrow() => {},
                }
                let recording = *receiver.borrow_and_update();
                if let Err(error) = controller.update(recording).await {
                    tracing::warn!(%error, "Could not update recording output volume");
                }
            }
            for _ in 0..3 {
                if controller.saved.is_none() {
                    break;
                }
                if let Err(error) = controller.update(false).await {
                    tracing::warn!(%error, "Could not restore recording output volume");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        });
        Self {
            requested: Some(requested),
            task: Some(task),
        }
    }

    pub fn set_recording(&self, recording: bool) {
        if let Some(sender) = &self.requested {
            sender.send_replace(recording);
        }
    }

    pub async fn shutdown(mut self) {
        self.requested.take();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
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
}
