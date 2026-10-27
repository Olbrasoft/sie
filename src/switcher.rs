//! Rust selection policy with an owned GTK view and existing GNOME window calls.
use crate::{desktop, lighting::DialFeedback};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command as Process},
    sync::{mpsc, watch},
    task::JoinHandle,
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

async fn windows(connection: &Connection) -> Result<Vec<Window>> {
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
#[derive(Deserialize)]
struct Message {
    event: String,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    revision: u64,
}
struct UiEvent {
    epoch: u64,
    message: Message,
}

struct Selection {
    index: usize,
    len: usize,
    revision: u64,
}

impl Selection {
    fn turn(&mut self, delta: i32) -> serde_json::Value {
        self.index = step(self.index, self.len, delta).unwrap_or(0);
        self.revision = self.revision.wrapping_add(1);
        serde_json::json!({"selected":self.index,"revision":self.revision})
    }

    fn observe(&mut self, message: &Message) {
        // Programmatic selection echoes may arrive after another turn.
        if message.revision == self.revision && message.index < self.len {
            self.index = message.index;
        }
    }
}

struct Picker {
    child: Child,
    stdin: ChildStdin,
    reader: JoinHandle<()>,
    epoch: u64,
    items: Vec<Window>,
    selection: Selection,
}

impl Picker {
    async fn send(&mut self, value: serde_json::Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(1), self.stdin.write_all(&bytes)).await??;
        Ok(())
    }
    async fn close(mut self) {
        let _ = self.child.kill().await;
        self.reader.abort();
    }

    async fn open(
        connection: &Connection,
        epoch: u64,
        events: mpsc::Sender<UiEvent>,
    ) -> Result<Self> {
        let mut items = choices(&windows(connection).await?, None, None);
        ensure!(!items.is_empty(), "No application windows to select");
        items.sort_by_key(|w| (w.workspace.unwrap_or(-1), w.wm_class.to_lowercase(), w.id));
        let selected = items.iter().position(|w| w.focus).unwrap_or(0);
        let mut child = Process::new("/usr/bin/python3")
            .args(["-u", "-c", include_str!("../assets/dial-picker.py")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id().context("Missing picker process")?;
        let stdin = child.stdin.take().context("Missing picker input")?;
        let stdout = child.stdout.take().context("Missing picker events")?;
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.len() > 4096 {
                    break;
                }
                if let Ok(message) = serde_json::from_str(&line)
                    && events.send(UiEvent { epoch, message }).await.is_err()
                {
                    return;
                }
            }
            let _ = events
                .send(UiEvent {
                    epoch,
                    message: Message {
                        event: "cancel".into(),
                        index: 0,
                        revision: 0,
                    },
                })
                .await;
        });
        let mut picker = Self {
            child,
            stdin,
            reader,
            epoch,
            selection: Selection {
                index: selected,
                len: items.len(),
                revision: 0,
            },
            items,
        };
        // Only display metadata is passed to the view; no typing is inspected.
        let rows: Vec<_> = picker
            .items
            .iter()
            .map(|w| {
                serde_json::json!({
                    "wm_class": w.wm_class,
                    "title": w.title.chars().take(240).collect::<String>(),
                    "workspace": w.workspace.unwrap_or(0)
                })
            })
            .collect();
        picker
            .send(serde_json::json!({"windows":rows,"selected":selected}))
            .await?;
        for _ in 0..40 {
            if let Some(window) = windows(connection).await?.iter().find(|w| w.pid == pid) {
                activate(connection, window.id, None).await?;
                return Ok(picker);
            }
            if picker.child.try_wait()?.is_some() {
                anyhow::bail!("Application picker exited during startup");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("Application picker did not appear")
    }
}

pub async fn run(
    connection: Connection,
    mut requests: mpsc::Receiver<Command>,
    feedback: watch::Sender<DialFeedback>,
) {
    let (ui_events, mut ui) = mpsc::channel::<UiEvent>(16);
    let mut picker: Option<Picker> = None;
    let mut epoch = 0u64;
    let mut green_until: Option<Instant> = None;
    loop {
        let event = tokio::select! {
            biased;
            event=ui.recv()=>event.map(Ok),
            request=requests.recv()=>match request {Some(request)=>Some(Err(request)),None=>break},
            _=async {if let Some(at)=green_until {tokio::time::sleep_until(at).await;} else {std::future::pending::<()>().await;}}=>{
                green_until=None;
                feedback.send_replace(DialFeedback::Idle);
                continue;
            },
        };
        let result: Result<()> = async {
            match event {
                Some(Err(Command::Turn(delta))) if picker.is_none() => {
                    let workspace = desktop::current_workspace(&connection).await?;
                    let list = choices(&windows(&connection).await?, Some(workspace), None);
                    if let Some(id) = local_target(&list, delta) {
                        feedback.send_replace(DialFeedback::Local);
                        activate(&connection, id, Some(workspace)).await?;
                        green_until = Some(Instant::now() + Duration::from_millis(150));
                    }
                }
                Some(Err(Command::Turn(delta))) => {
                    let picker = picker.as_mut().unwrap();
                    let update = picker.selection.turn(delta);
                    picker.send(update).await?;
                }
                Some(Err(Command::Click)) => {
                    if let Some(picker) = &mut picker {
                        picker.send(serde_json::json!({"confirm":true})).await?;
                    } else {
                        green_until = None;
                        epoch = epoch.wrapping_add(1);
                        feedback.send_replace(DialFeedback::Global);
                        picker = Some(Picker::open(&connection, epoch, ui_events.clone()).await?);
                    }
                }
                Some(Err(Command::Cancel)) => {
                    if let Some(picker) = picker.take() {
                        picker.close().await;
                    }
                    green_until = None;
                    feedback.send_replace(DialFeedback::Idle);
                }
                Some(Ok(event)) if picker.as_ref().is_some_and(|p| p.epoch == event.epoch) => {
                    match event.message.event.as_str() {
                        "selected" => {
                            picker.as_mut().unwrap().selection.observe(&event.message);
                        }
                        "confirm" => {
                            let p = picker.take().unwrap();
                            let focused = windows(&connection)
                                .await?
                                .iter()
                                .any(|w| w.focus && Some(w.pid) == p.child.id());
                            let target = p.items.get(event.message.index).map(|w| w.id);
                            p.close().await;
                            if focused && let Some(id) = target {
                                activate(&connection, id, None).await?;
                            }
                            feedback.send_replace(DialFeedback::Idle);
                        }
                        "cancel" => {
                            picker.take().unwrap().close().await;
                            feedback.send_replace(DialFeedback::Idle);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error,"Command Dial switching unavailable");
            if let Some(picker) = picker.take() {
                picker.close().await;
            }
            green_until = None;
            feedback.send_replace(DialFeedback::Idle);
        }
    }
    if let Some(picker) = picker {
        picker.close().await;
    }
    feedback.send_replace(DialFeedback::Idle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_global_turns_ignore_stale_view_echoes_but_accept_current_mouse_selection() {
        let mut selection = Selection {
            index: 0,
            len: 5,
            revision: 0,
        };
        assert_eq!(selection.turn(1)["selected"], 1);
        assert_eq!(selection.turn(1)["selected"], 2);
        let stale = serde_json::from_str(r#"{"event":"selected","index":1,"revision":1}"#).unwrap();
        selection.observe(&stale);
        assert_eq!(selection.turn(1)["selected"], 3);
        let mouse = serde_json::from_str(r#"{"event":"selected","index":0,"revision":3}"#).unwrap();
        selection.observe(&mouse);
        assert_eq!(selection.turn(-1)["selected"], 4);
        let invalid =
            serde_json::from_str(r#"{"event":"selected","index":99,"revision":4}"#).unwrap();
        selection.observe(&invalid);
        assert_eq!(selection.turn(-1)["selected"], 3);
    }

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
