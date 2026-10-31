//! Shared attenuation of one media output for recording and notification speech.
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
    running: bool,
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
        #[serde(default)]
        state: String,
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
        running: raw.state == "RUNNING",
        volumes,
    }))
}

async fn pactl(args: &[String]) -> Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_millis(500),
        Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C.UTF-8")
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
    uncertain: bool,
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
        self.update_requested(recording, false).await
    }

    async fn update_requested(&mut self, recording: bool, notification: bool) -> Result<()> {
        if recording || notification {
            if let Some(saved) = &mut self.saved {
                if saved.uncertain {
                    if let Some(current) = self.control.read(&self.sink).await?
                        && saved.original.same_output(&current)
                        && same_volume(&current.volumes, &saved.original.volumes)
                    {
                        self.control.write(&current, &saved.applied).await?;
                    }
                    saved.uncertain = false;
                }
                return Ok(());
            }
            let Some(original) = self.control.read(&self.sink).await? else {
                return Ok(());
            };
            if !recording && notification && !original.running {
                return Ok(());
            }
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
            let saved = self.saved.insert(Saved {
                original,
                applied,
                uncertain: true,
            });
            self.control.write(&saved.original, &saved.applied).await?;
            saved.uncertain = false;
            tracing::info!(output = %self.sink, "Media output attenuated for voice activity");
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

#[derive(Clone, Copy, Default)]
struct Requests {
    recording: bool,
    notifications: u32,
    epoch: u64,
    shutdown: bool,
}
impl Requests {
    fn active(self) -> bool {
        self.recording || self.notifications > 0
    }
    fn changed(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Applied {
    epoch: u64,
    success: bool,
}

#[derive(Clone)]
struct Channels {
    requested: watch::Sender<Requests>,
    applied: watch::Receiver<Applied>,
}

#[derive(Clone, Default)]
pub(crate) struct Handle(Option<Channels>);
impl Handle {
    pub fn recording(&self) -> RecordingClient {
        RecordingClient {
            handle: self.clone(),
        }
    }
    fn set_recording(&self, recording: bool) {
        if let Some(channels) = &self.0 {
            let sender = &channels.requested;
            sender.send_if_modified(|r| {
                if r.recording == recording || r.shutdown {
                    false
                } else {
                    r.recording = recording;
                    r.changed();
                    true
                }
            });
        }
    }
    pub async fn notification(&self) -> Result<NotificationLease> {
        let mut lease = NotificationLease { requested: None };
        if let Some(channels) = &self.0 {
            let sender = &channels.requested;
            let mut epoch = None;
            sender.send_if_modified(|r| {
                if r.shutdown {
                    return false;
                }
                r.notifications = r.notifications.saturating_add(1);
                r.changed();
                epoch = Some(r.epoch);
                true
            });
            let epoch = epoch.context("Media attenuation controller stopped")?;
            lease.requested = Some(sender.clone());
            let mut applied = channels.applied.clone();
            let ready = tokio::time::timeout(
                Duration::from_secs(2),
                applied.wait_for(|a| a.epoch >= epoch),
            )
            .await??;
            ensure!(
                ready.success,
                "Could not attenuate the media output before speech"
            );
        }
        Ok(lease)
    }
}
pub(crate) struct RecordingClient {
    handle: Handle,
}
impl RecordingClient {
    pub fn set_recording(&self, recording: bool) {
        self.handle.set_recording(recording);
    }
}
impl Drop for RecordingClient {
    fn drop(&mut self) {
        self.handle.set_recording(false);
    }
}
pub(crate) struct NotificationLease {
    requested: Option<watch::Sender<Requests>>,
}
impl Drop for NotificationLease {
    fn drop(&mut self) {
        if let Some(sender) = &self.requested {
            sender.send_modify(|r| {
                r.notifications = r.notifications.saturating_sub(1);
                r.changed();
            });
        }
    }
}
pub(crate) struct Ducking {
    handle: Handle,
    task: Option<JoinHandle<()>>,
}
impl Ducking {
    pub fn new(sink: Option<String>, percent: u8) -> Self {
        match sink {
            Some(sink) => Self::spawn(Pactl, sink, percent),
            None => Self {
                handle: Handle::default(),
                task: None,
            },
        }
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
    #[cfg(test)]
    pub fn set_recording(&self, recording: bool) {
        self.handle.set_recording(recording);
    }
    fn spawn(control: impl Control + 'static, sink: String, percent: u8) -> Self {
        let (requested, mut receiver) = watch::channel(Requests::default());
        let (applied, applied_rx) = watch::channel(Applied::default());
        let task = tokio::spawn(async move {
            let mut controller = Controller {
                control,
                sink,
                percent,
                saved: None,
            };
            loop {
                tokio::select! {
                    changed=receiver.changed()=>if changed.is_err(){break;},
                    _=tokio::time::sleep(Duration::from_millis(250)),if controller.saved.is_some() || receiver.borrow().active()=>{},
                }
                let request = *receiver.borrow_and_update();
                if request.shutdown {
                    break;
                }
                let result = controller
                    .update_requested(request.recording, request.notifications > 0)
                    .await;
                let next = Applied {
                    epoch: request.epoch,
                    success: result.is_ok(),
                };
                applied.send_if_modified(|old| {
                    if *old == next {
                        false
                    } else {
                        *old = next;
                        true
                    }
                });
                if let Err(error) = result {
                    tracing::warn!(%error,"Could not update voice attenuation");
                }
            }
            for _ in 0..3 {
                if controller.saved.is_none() {
                    break;
                }
                if let Err(error) = controller.update(false).await {
                    tracing::warn!(%error,"Could not restore voice attenuation");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        });
        Self {
            handle: Handle(Some(Channels {
                requested,
                applied: applied_rx,
            })),
            task: Some(task),
        }
    }
    fn stop(&self) {
        if let Some(channels) = &self.handle.0 {
            channels.requested.send_modify(|r| {
                r.shutdown = true;
                r.changed();
            });
        }
    }
    pub async fn shutdown(mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Ducking {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
