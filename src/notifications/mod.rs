//! Durable, serial spoken notifications owned by the existing desktop process.
mod api;
pub mod config;
mod process;
pub mod store;
mod tts;

use anyhow::{Result, ensure};
use config::{Options, Settings};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use store::Store;
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub text: String,
    pub source: String,
    #[serde(default)]
    pub issue_ids: Vec<i32>,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.text.trim().is_empty() && self.text.encode_utf16().count() <= 4000,
            "Text must contain 1–4000 characters"
        );
        ensure!(
            !self.source.trim().is_empty()
                && self.source.len() <= 160
                && !self.source.chars().any(char::is_control),
            "Invalid notification source"
        );
        ensure!(
            self.issue_ids.len() <= 20 && self.issue_ids.iter().all(|id| *id > 0),
            "Invalid issue references"
        );
        for field in [&self.provider_name, &self.model_name]
            .into_iter()
            .flatten()
        {
            ensure!(field.len() <= 160, "Notification metadata is too long");
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct Gate {
    pub recording: bool,
    pub muted: bool,
    pub stop: u64,
    pub external_until: Option<Instant>,
    pub shutdown: bool,
}
impl Gate {
    pub fn blocked(&self) -> bool {
        self.recording || self.muted || self.external_until.is_some_and(|v| v > Instant::now())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub speaking: bool,
    pub pending: i64,
    pub muted: bool,
    pub locked: bool,
    pub unavailable: bool,
}

pub struct Control {
    pub(crate) gate: watch::Sender<Gate>,
    pub(crate) wake: Notify,
    status: watch::Sender<Status>,
    pub(crate) ducking: crate::ducking::Handle,
}

impl Control {
    #[cfg(test)]
    fn new() -> Arc<Self> {
        Self::with_ducking(crate::ducking::Handle::default())
    }
    fn with_ducking(ducking: crate::ducking::Handle) -> Arc<Self> {
        Arc::new(Self {
            ducking,
            gate: watch::channel(Gate::default()).0,
            wake: Notify::new(),
            status: watch::channel(Status::default()).0,
        })
    }
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }
    pub fn toggle_mute(&self) {
        self.gate.send_modify(|g| g.muted = !g.muted);
    }
    pub fn stop_current(&self) {
        self.gate.send_modify(|g| g.stop = g.stop.wrapping_add(1));
    }
    pub async fn recording(&self, active: bool) -> Result<()> {
        self.gate.send_if_modified(|g| {
            if g.recording == active {
                false
            } else {
                g.recording = active;
                true
            }
        });
        if active {
            self.wait_silent().await?;
        }
        Ok(())
    }
    pub(crate) async fn wait_silent(&self) -> Result<()> {
        let mut status = self.subscribe();
        tokio::time::timeout(Duration::from_secs(2), status.wait_for(|s| !s.speaking)).await??;
        Ok(())
    }
    fn publish(&self, speaking: bool, pending: i64, unavailable: bool) {
        let gate = self.gate.borrow();
        let next = Status {
            speaking,
            pending,
            muted: gate.muted,
            locked: gate.recording || gate.external_until.is_some_and(|v| v > Instant::now()),
            unavailable,
        };
        self.status.send_if_modified(|old| {
            if *old == next {
                false
            } else {
                *old = next;
                true
            }
        });
    }
}

pub struct Runtime {
    pub control: Arc<Control>,
    tasks: Vec<JoinHandle<()>>,
    ducking: Option<crate::ducking::Ducking>,
}

impl Runtime {
    pub async fn start(
        options: &Options,
        database_url: &str,
        duck_sink: Option<String>,
        duck_percent: u8,
    ) -> Result<Self> {
        let settings = Settings::load(options)?;
        let store = Arc::new(Store::connect(database_url, options.max_pending).await?);
        store.own_consumer().await?;
        let listener=tokio::net::TcpListener::bind(options.listen).await
            .map_err(|_|anyhow::anyhow!("Cannot bind the notification API; stop VirtualAssistant before switching the shared port"))?;
        let chain = tts::Chain::new(settings.clone(), store.clone()).await?;
        ensure!(
            duck_sink.as_deref() != Some(settings.sound_sink.as_str()),
            "Speech and attenuated media outputs must differ"
        );
        let ducking = crate::ducking::Ducking::new(duck_sink, duck_percent);
        let control = Control::with_ducking(ducking.handle());
        control.publish(false, store.count().await?, false);
        let credentials = settings
            .providers
            .iter()
            .filter(|p| p.name == "GoogleCloudMultiKey")
            .flat_map(|p| p.credentials.clone())
            .collect();
        let app = api::router(store.clone(), control.clone(), credentials);
        let mut stopped = control.gate.subscribe();
        let server = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|g| g.shutdown).await;
                })
                .await;
            if result.is_err() {
                tracing::error!("Notification API stopped unexpectedly");
            }
        });
        let worker = tokio::spawn(worker(
            store,
            control.clone(),
            NativeSpeaker {
                chain,
                sink: settings.sound_sink,
                ducking: ducking.handle(),
                cached: None,
            },
        ));
        Ok(Self {
            control,
            tasks: vec![server, worker],
            ducking: Some(ducking),
        })
    }
    pub fn request_shutdown(&self) {
        self.control.gate.send_modify(|g| g.shutdown = true);
    }
    pub async fn shutdown(mut self) {
        self.request_shutdown();
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        if let Some(ducking) = self.ducking.take() {
            ducking.shutdown().await;
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.control.gate.send_modify(|g| g.shutdown = true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

trait Speaker: Send {
    fn speak(&mut self, request: &Request) -> impl std::future::Future<Output = Result<()>> + Send;
    fn discard(&mut self) {}
}
struct NativeSpeaker {
    chain: tts::Chain,
    sink: String,
    cached: Option<(String, String, Vec<u8>)>,
    ducking: crate::ducking::Handle,
}
impl Speaker for NativeSpeaker {
    async fn speak(&mut self, request: &Request) -> Result<()> {
        if self
            .cached
            .as_ref()
            .is_none_or(|(source, text, _)| source != &request.source || text != &request.text)
        {
            let audio = self
                .chain
                .synthesize(&request.text, &request.source)
                .await?;
            self.cached = Some((request.source.clone(), request.text.clone(), audio));
        }
        let result =
            process::play(&self.cached.as_ref().unwrap().2, &self.sink, &self.ducking).await;
        self.cached = None;
        result
    }
    fn discard(&mut self) {
        self.cached = None;
    }
}

async fn worker<B: Speaker>(store: Arc<Store>, control: Arc<Control>, mut speaker: B) {
    let mut gate = control.gate.subscribe();
    loop {
        if gate.borrow().shutdown {
            break;
        }
        let count = store.count().await;
        control.publish(false, count.as_ref().copied().unwrap_or(0), count.is_err());
        if !gate.borrow().blocked() {
            match store.next().await {
                Ok(Some(job)) => {
                    // Mark the watch value consumed immediately before starting
                    // work, then recheck it so no recording transition is lost.
                    let before = gate.borrow_and_update().clone();
                    if before.blocked() || before.shutdown {
                        continue;
                    }
                    control.publish(true, count.as_ref().copied().unwrap_or(0), false);
                    let result = tokio::select! {
                        biased;
                        _=async {
                            loop {
                                if gate.changed().await.is_err() {break;}
                                let current=gate.borrow();
                                if current.blocked() || current.shutdown || current.stop!=before.stop {break;}
                            }
                        }=>None,
                        result=speaker.speak(&job.request)=>Some(result),
                    };
                    // Drop any owned synthesis/player child before reporting
                    // silence to the recording coordinator.
                    control.publish(false, count.as_ref().copied().unwrap_or(0), false);
                    let stopped = gate.borrow().stop != before.stop;
                    if stopped {
                        speaker.discard();
                    }
                    let update = match result {
                        Some(Ok(())) => store.finish(job.id).await,
                        None if stopped => store.finish(job.id).await,
                        None => Ok(()), // Retain interrupted speech for retry.
                        Some(Err(_)) => {
                            tracing::warn!(notification = job.id, "Notification speech will retry");
                            store.retry(job.id).await
                        }
                    };
                    if update.is_err() {
                        tracing::warn!(notification = job.id, "Notification queue update failed");
                    }
                    control.publish(false, store.count().await.unwrap_or(0), update.is_err());
                    continue;
                }
                Ok(None) => {}
                Err(_) => {
                    tracing::warn!("Notification queue unavailable; reconnecting on next poll")
                }
            }
        }
        tokio::select! {
            biased;
            _=gate.changed()=>{},
            _=control.wake.notified()=>{},
            _=tokio::time::sleep(Duration::from_millis(250))=>{},
        }
    }
    control.publish(false, store.count().await.unwrap_or(0), false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatible_payload_limits_do_not_expose_content_in_errors() {
        let mut r:Request=serde_json::from_value(serde_json::json!({"text":"Ahoj","source":"codex","issueIds":[85],"providerName":"openai","modelName":"codex"})).unwrap();
        assert!(r.validate().is_ok());
        r.text = "😀".repeat(2001);
        assert!(r.validate().is_err());
        r.text = "Test".into();
        r.issue_ids = vec![0];
        assert!(r.validate().is_err());
    }
    #[tokio::test]
    async fn recording_and_external_lock_are_independent() {
        let c = Control::new();
        c.recording(true).await.unwrap();
        c.gate.send_modify(|g| g.external_until = None);
        assert!(c.gate.borrow().blocked());
        c.recording(false).await.unwrap();
        assert!(!c.gate.borrow().blocked());
        c.toggle_mute();
        assert!(c.gate.borrow().blocked());
    }
}

#[cfg(test)]
mod integration_tests;
