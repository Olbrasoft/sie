//! A scoped client for the native GNOME application switcher and window activation.
use crate::{desktop, lighting::DialFeedback};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use zbus::{Connection, Proxy};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Window {
    pub id: u32,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub wm_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub workspace: Option<i32>,
    #[serde(default)]
    pub in_current_workspace: bool,
    #[serde(default)]
    pub window_type: u32,
    #[serde(default)]
    pub focus: bool,
}

pub(crate) async fn windows(connection: &Connection) -> Result<Vec<Window>> {
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    let json: String = proxy.call("List", &()).await?;
    let values: Vec<Window> = serde_json::from_str(&json)?;
    Ok(values)
}

pub async fn activate(connection: &Connection, id: u32, workspace: Option<i32>) -> Result<()> {
    let before = windows(connection).await?;
    let target = before
        .iter()
        .find(|w| w.id == id)
        .context("Selected window closed")?;
    if let Some(index) = workspace {
        ensure!(
            desktop::current_workspace(connection).await? == index
                && target.workspace == Some(index),
            "Workspace changed before local switching"
        );
    }
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    proxy.call::<_, _, ()>("Activate", &(id,)).await?;
    for _ in 0..20 {
        if windows(connection)
            .await?
            .iter()
            .any(|w| w.id == id && w.focus)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("Selected window did not become active")
}

#[derive(Debug, Clone, Copy)]
pub enum Command {
    Turn(i32),
    Click,
    Cancel,
}
pub const NATIVE_BUS: &str = "cz.olbrasoft.SIE.Switcher";
pub const NATIVE_PATH: &str = "/cz/olbrasoft/SIE/Switcher";
const NATIVE_TIMEOUT: Duration = Duration::from_millis(750);
const NATIVE_PROTOCOL_VERSION: u32 = 3;

pub async fn available(connection: &Connection) -> bool {
    native_version(connection).await == Some(NATIVE_PROTOCOL_VERSION)
}

async fn native_version(connection: &Connection) -> Option<u32> {
    tokio::time::timeout(NATIVE_TIMEOUT, async {
        let proxy = native_proxy(connection).await?;
        let version: u32 = proxy.call("Version", &()).await?;
        Ok::<_, anyhow::Error>(version)
    })
    .await
    .ok()
    .and_then(Result::ok)
}

struct NativePicker {
    token: u32,
}

async fn native_proxy(connection: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(connection, NATIVE_BUS, NATIVE_PATH, NATIVE_BUS).await?)
}

impl NativePicker {
    async fn open(connection: &Connection, initial_delta: Option<i32>) -> Result<Self> {
        let version = native_version(connection).await;
        ensure!(
            version == Some(NATIVE_PROTOCOL_VERSION)
                || (initial_delta.is_none() && version == Some(2)),
            "The native switcher adapter needs activation or an update; log out and back in"
        );
        let proxy = native_proxy(connection).await?;
        let token: u32 = tokio::time::timeout(NATIVE_TIMEOUT, async {
            if let Some(delta) = initial_delta {
                proxy.call("OpenLocal", &(delta,)).await
            } else {
                proxy.call("Open", &()).await
            }
        })
        .await?
        .context("Enable the SIE Native Application Switcher GNOME extension; first installation requires logout/login")?;
        ensure!(token != 0, "Native switcher returned an invalid token");
        Ok(Self { token })
    }

    async fn turn(&self, connection: &Connection, delta: i32) -> Result<bool> {
        let proxy = native_proxy(connection).await?;
        Ok(
            tokio::time::timeout(NATIVE_TIMEOUT, proxy.call("Move", &(self.token, delta)))
                .await??,
        )
    }

    async fn active(&self, connection: &Connection) -> Result<bool> {
        let proxy = native_proxy(connection).await?;
        Ok(tokio::time::timeout(NATIVE_TIMEOUT, proxy.call("Active", &(self.token,))).await??)
    }

    async fn close(self, connection: &Connection, confirm: bool) -> Result<()> {
        let proxy = native_proxy(connection).await?;
        let _: bool =
            tokio::time::timeout(NATIVE_TIMEOUT, proxy.call("Finish", &(self.token, confirm)))
                .await??;
        Ok(())
    }
}

pub async fn run(
    connection: Connection,
    mut requests: mpsc::Receiver<Command>,
    feedback: watch::Sender<DialFeedback>,
) {
    let mut picker: Option<NativePicker> = None;
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            biased;
            request=requests.recv()=>match request {Some(request)=>Some(request),None=>break},
            _=poll.tick(), if picker.is_some()=>None,
        };
        if matches!(command, Some(Command::Turn(0))) {
            continue;
        }
        let result: Result<()> = async {
            // A native Enter/mouse/Escape action can close the popup between
            // polling ticks. Treat the next dial action as a new interaction.
            if let Some(active) = &picker
                && !active.active(&connection).await?
            {
                picker = None;
                feedback.send_replace(DialFeedback::Idle);
            }
            match command {
                Some(Command::Turn(delta)) if picker.is_none() => {
                    picker = Some(NativePicker::open(&connection, Some(delta)).await?);
                    feedback.send_replace(DialFeedback::Local);
                }
                Some(Command::Turn(delta)) => {
                    if !picker.as_ref().unwrap().turn(&connection, delta).await? {
                        picker = None;
                        feedback.send_replace(DialFeedback::Idle);
                    }
                }
                Some(Command::Click) => {
                    if let Some(picker) = picker.take() {
                        picker.close(&connection, true).await?;
                        feedback.send_replace(DialFeedback::Idle);
                    } else {
                        picker = Some(NativePicker::open(&connection, None).await?);
                        feedback.send_replace(DialFeedback::Global);
                    }
                }
                Some(Command::Cancel) => {
                    if let Some(picker) = picker.take() {
                        picker.close(&connection, false).await?;
                    }
                    feedback.send_replace(DialFeedback::Idle);
                }
                None => {}
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(error=%format!("{error:#}"), "Command Dial switching unavailable");
            if let Some(picker) = picker.take() {
                let _ = picker.close(&connection, false).await;
            }
            feedback.send_replace(DialFeedback::Idle);
        }
    }
    if let Some(picker) = picker {
        let _ = picker.close(&connection, false).await;
    }
    feedback.send_replace(DialFeedback::Idle);
}
