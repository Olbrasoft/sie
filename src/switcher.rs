//! Local window policy and a client for the native GNOME application switcher.
use crate::{desktop, lighting::DialFeedback};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
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

pub fn choices(windows: &[Window], workspace: Option<i32>, excluded: Option<u32>) -> Vec<Window> {
    let mut seen = HashSet::new();
    let mut selected: Vec<_> = windows
        .iter()
        .filter(|w| {
            w.id != 0
                && !w.wm_class.is_empty()
                && w.wm_class != "sie-switcher"
                && matches!(w.window_type, 0 | 3 | 4)
                && excluded.is_none_or(|pid| w.pid != pid)
                && workspace.is_none_or(|index| w.workspace == Some(index))
                && seen.insert(w.id)
        })
        .take(128)
        .cloned()
        .collect();
    selected.sort_by_key(|w| w.id);
    selected
}

pub fn step(index: usize, len: usize, delta: i32) -> Option<usize> {
    (len > 0).then(|| (index as i64 + i64::from(delta)).rem_euclid(len as i64) as usize)
}

pub fn local_target(windows: &[Window], delta: i32) -> Option<u32> {
    if windows.is_empty() || delta == 0 || (windows.len() == 1 && windows[0].focus) {
        return None;
    }
    let index = windows.iter().position(|w| w.focus);
    let next = if let Some(index) = index {
        step(index, windows.len(), delta)?
    } else if delta > 0 {
        0
    } else {
        windows.len() - 1
    };
    Some(windows[next].id)
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
const NATIVE_PROTOCOL_VERSION: u32 = 2;

pub async fn available(connection: &Connection) -> bool {
    tokio::time::timeout(NATIVE_TIMEOUT, async {
        let proxy = native_proxy(connection).await?;
        let version: u32 = proxy.call("Version", &()).await?;
        Ok::<_, anyhow::Error>(version == NATIVE_PROTOCOL_VERSION)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(false)
}

struct NativePicker {
    token: u32,
}

async fn native_proxy(connection: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(connection, NATIVE_BUS, NATIVE_PATH, NATIVE_BUS).await?)
}

impl NativePicker {
    async fn open(connection: &Connection) -> Result<Self> {
        ensure!(
            available(connection).await,
            "The native switcher adapter needs activation or an update; log out and back in"
        );
        let proxy = native_proxy(connection).await?;
        let token: u32 = tokio::time::timeout(NATIVE_TIMEOUT, proxy.call("Open", &()))
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
    let mut green_until: Option<Instant> = None;
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            biased;
            request=requests.recv()=>match request {Some(request)=>Some(request),None=>break},
            _=poll.tick(), if picker.is_some()=>None,
            _=async {if let Some(at)=green_until {tokio::time::sleep_until(at).await;} else {std::future::pending::<()>().await;}}=>{
                green_until=None;
                feedback.send_replace(DialFeedback::Idle);
                continue;
            },
        };
        let result: Result<()> = async {
            match command {
                Some(Command::Turn(delta)) if picker.is_none() => {
                    let workspace = desktop::current_workspace(&connection).await?;
                    let list = choices(&windows(&connection).await?, Some(workspace), None);
                    if let Some(id) = local_target(&list, delta) {
                        feedback.send_replace(DialFeedback::Local);
                        activate(&connection, id, Some(workspace)).await?;
                        green_until = Some(Instant::now() + Duration::from_millis(150));
                    }
                }
                Some(Command::Turn(delta)) => {
                    if !picker.as_ref().unwrap().turn(&connection, delta).await? {
                        picker = None;
                        feedback.send_replace(DialFeedback::Idle);
                    }
                }
                Some(Command::Click) => {
                    green_until = None;
                    if let Some(picker) = picker.take() {
                        picker.close(&connection, true).await?;
                        feedback.send_replace(DialFeedback::Idle);
                    } else {
                        picker = Some(NativePicker::open(&connection).await?);
                        feedback.send_replace(DialFeedback::Global);
                    }
                }
                Some(Command::Cancel) => {
                    if let Some(picker) = picker.take() {
                        picker.close(&connection, false).await?;
                    }
                    green_until = None;
                    feedback.send_replace(DialFeedback::Idle);
                }
                None => {
                    if !picker.as_ref().unwrap().active(&connection).await? {
                        picker = None;
                        feedback.send_replace(DialFeedback::Idle);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(error=%format!("{error:#}"), "Command Dial switching unavailable");
            if let Some(picker) = picker.take() {
                let _ = picker.close(&connection, false).await;
            }
            green_until = None;
            feedback.send_replace(DialFeedback::Idle);
        }
    }
    if let Some(picker) = picker {
        let _ = picker.close(&connection, false).await;
    }
    feedback.send_replace(DialFeedback::Idle);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u32, workspace: Option<i32>, focus: bool) -> Window {
        Window {
            id,
            pid: id,
            wm_class: "Application".into(),
            title: "Title".into(),
            workspace,
            in_current_workspace: false,
            window_type: 0,
            focus,
        }
    }

    #[test]
    fn local_choices_cannot_escape_the_current_workspace_even_with_stale_flags() {
        let mut stale = window(4, Some(4), false);
        stale.in_current_workspace = true;
        let list = vec![
            window(3, Some(0), true),
            window(2, Some(0), false),
            window(1, None, false),
            stale,
        ];
        let local = choices(&list, Some(0), None);
        assert_eq!(local.iter().map(|w| w.id).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(choices(&list, None, None).len(), 4);
        assert!(choices(&list, Some(8), None).is_empty());
    }

    #[test]
    fn selection_excludes_desktop_popup_own_picker_invalid_and_duplicate_windows() {
        let mut list = vec![window(1, Some(0), true), window(1, Some(0), false)];
        for (id, window_type) in [(2, 1), (3, 2), (4, 7)] {
            let mut w = window(id, Some(0), false);
            w.window_type = window_type;
            list.push(w);
        }
        let mut own = window(5, Some(0), false);
        own.wm_class = "sie-switcher".into();
        list.extend([own, window(0, Some(0), false)]);
        for (id, window_type) in [(6, 3), (7, 4)] {
            let mut w = window(id, Some(0), false);
            w.window_type = window_type;
            list.push(w);
        }
        assert_eq!(
            choices(&list, None, None)
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            [1, 6, 7]
        );
        assert_eq!(choices(&list, None, Some(1)).len(), 2);
    }

    #[test]
    fn stable_order_cycles_both_directions_instead_of_bouncing_with_focus_order() {
        let list = vec![
            window(30, Some(0), true),
            window(10, Some(0), false),
            window(20, Some(0), false),
        ];
        let mut sorted = choices(&list, Some(0), None);
        assert_eq!(local_target(&sorted, 1), Some(10));
        assert_eq!(local_target(&sorted, -1), Some(20));
        for w in &mut sorted {
            w.focus = w.id == 10;
        }
        assert_eq!(local_target(&sorted, 1), Some(20));
        assert_eq!(local_target(&sorted, -1), Some(30));
        assert_eq!(step(0, 3, -4), Some(2));
        assert_eq!(step(2, 3, 5), Some(1));
        assert_eq!(step(0, 0, 1), None);
        assert_eq!(local_target(&[], 1), None);
        assert_eq!(local_target(&[window(1, Some(0), true)], 1), None);
        assert_eq!(local_target(&sorted, 0), None);
        assert_eq!(local_target(&[window(1, Some(0), false)], 1), Some(1));
    }
}
