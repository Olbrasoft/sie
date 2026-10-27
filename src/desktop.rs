use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zbus::{Connection, Proxy, zvariant::OwnedObjectPath};

use crate::macros::workspace_exists;

const TRACKER: &str = "org.olbrasoft.Desktop";
const MUTTER: &str = "org.gnome.Mutter.RemoteDesktop";
const SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
// Mutter NotifyKeyboardKeycode takes Linux evdev codes, not X11 keycodes.
const SUPER_L: u32 = 125;
const WORKSPACE_KEYS: [u32; 5] = [79, 80, 81, 75, 76];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    PasteOnly,
    Submit,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceState {
    pub index: i32,
    pub count: i32,
}

impl WorkspaceState {
    pub fn active_macro(&self) -> Option<i32> {
        (self.index >= 0 && self.index < 5 && self.index < self.count).then_some(self.index)
    }
}

pub async fn workspace_state(connection: &Connection) -> Result<WorkspaceState> {
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(TRACKER)?
        .path("/org/olbrasoft/Desktop")?
        .build()
        .await?;
    let mut values = properties.get_all(TRACKER.try_into()?).await?;
    Ok(WorkspaceState {
        index: i32::try_from(
            values
                .remove("CurrentWorkspace")
                .context("Missing current workspace")?,
        )?,
        count: i32::try_from(
            values
                .remove("TotalWorkspaces")
                .context("Missing workspace count")?,
        )?,
    })
}

/// Any workspace change triggers a fresh read, including mouse and other shortcuts.
pub async fn watch_workspaces(connection: Connection, changed: tokio::sync::mpsc::Sender<()>) {
    loop {
        let result: Result<()> = async {
            let proxy = Proxy::new(&connection, TRACKER, "/org/olbrasoft/Desktop", TRACKER).await?;
            let mut signals = proxy.receive_signal("WorkspaceChanged").await?;
            let _ = changed.try_send(());
            while signals.next().await.is_some() {
                if changed.is_closed() {
                    return Ok(());
                }
                // Coalesce rapid switches; the consumer reads the latest state.
                let _ = changed.try_send(());
            }
            bail!("Workspace signal stream closed")
        }
        .await;
        if changed.is_closed() {
            return;
        }
        if let Err(error) = result {
            tracing::debug!(%error, "Workspace signal subscription unavailable; retrying");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub async fn workspace_count(connection: &Connection) -> Result<i32> {
    workspace_property(connection, "TotalWorkspaces").await
}

pub async fn current_workspace(connection: &Connection) -> Result<i32> {
    workspace_property(connection, "CurrentWorkspace").await
}

async fn workspace_property(connection: &Connection, name: &str) -> Result<i32> {
    // Never cache this property: GNOME can add/remove dynamic workspaces at any time.
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(TRACKER)?
        .path("/org/olbrasoft/Desktop")?
        .build()
        .await?;
    let count = properties.get(TRACKER.try_into()?, name).await?;
    Ok(i32::try_from(count)?)
}

pub fn binding_matches(value: &str, index: u32) -> bool {
    index < 5
        && (value.contains(&format!("'<Super>KP_{}'", index + 1))
            || value.contains(&format!("\"<Super>KP_{}\"", index + 1)))
}

pub async fn check_binding(index: u32) -> Result<()> {
    let output = Command::new("gsettings")
        .args([
            "get",
            "org.gnome.desktop.wm.keybindings",
            &format!("switch-to-workspace-{}", index + 1),
        ])
        .kill_on_drop(true)
        .output()
        .await
        .context("Cannot execute gsettings")?;
    ensure!(
        output.status.success(),
        "Cannot read GNOME workspace keybinding"
    );
    let value = String::from_utf8(output.stdout)?;
    ensure!(
        binding_matches(&value, index),
        "Workspace {} requires the existing GNOME shortcut <Super>KP_{}; current value: {}",
        index,
        index + 1,
        value.trim()
    );
    Ok(())
}

/// A compositor-owned input session; disconnecting also releases virtual keys.
pub struct Desktop {
    connection: Connection,
    session: OwnedObjectPath,
}

impl Desktop {
    pub async fn connect(connection: &Connection) -> Result<Self> {
        let root = Proxy::new(
            connection,
            MUTTER,
            "/org/gnome/Mutter/RemoteDesktop",
            MUTTER,
        )
        .await?;
        let session: OwnedObjectPath = root.call("CreateSession", &()).await?;
        let desktop = Self {
            connection: connection.clone(),
            session,
        };
        desktop
            .proxy()
            .await?
            .call::<_, _, ()>("Start", &())
            .await?;
        Ok(desktop)
    }

    async fn proxy(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(&self.connection, MUTTER, self.session.as_str(), SESSION).await?)
    }

    pub async fn switch(&self, index: u32) -> Result<bool> {
        if index >= 5 {
            bail!("Only workspace indices 0 through 4 are mapped");
        }
        if !workspace_exists(index, workspace_count(&self.connection).await?) {
            return Ok(false);
        }
        // Validate the actual configured shortcut without changing user settings.
        check_binding(index).await?;
        let proxy = self.proxy().await?;
        proxy
            .call::<_, _, ()>("NotifyKeyboardKeycode", &(SUPER_L, true))
            .await?;
        let press = proxy
            .call::<_, _, ()>(
                "NotifyKeyboardKeycode",
                &(WORKSPACE_KEYS[index as usize], true),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Attempt both releases even if pressing or releasing the numpad key fails.
        let release_key = proxy
            .call::<_, _, ()>(
                "NotifyKeyboardKeycode",
                &(WORKSPACE_KEYS[index as usize], false),
            )
            .await;
        let release_super = proxy
            .call::<_, _, ()>("NotifyKeyboardKeycode", &(SUPER_L, false))
            .await;
        press?;
        release_key?;
        release_super?;
        // Mutter queues input asynchronously. Keep the session alive until the
        // compositor confirms activation rather than dropping pending events.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if current_workspace(&self.connection).await? == index as i32 {
                return Ok(true);
            }
        }
        bail!("GNOME did not activate workspace {index}; check shortcut conflicts and Num Lock")
    }

    pub async fn stop(self) -> Result<()> {
        self.proxy().await?.call::<_, _, ()>("Stop", &()).await?;
        Ok(())
    }

    pub async fn send_keys(&self, keys: &[u32]) -> Result<()> {
        let proxy = self.proxy().await?;
        let mut failure = None;
        for key in keys {
            if let Err(error) = proxy
                .call::<_, _, ()>("NotifyKeyboardKeycode", &(*key, true))
                .await
            {
                failure = Some(error);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        for key in keys.iter().rev() {
            if let Err(error) = proxy
                .call::<_, _, ()>("NotifyKeyboardKeycode", &(*key, false))
                .await
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error.into());
        }
        Ok(())
    }

    /// The selection is already staged; Enter is a separate, guarded action.
    pub async fn paste_shortcut(
        &self,
        keys: &[u32],
        target: &FocusTarget,
        mode: DeliveryMode,
        cancelled: &AtomicBool,
    ) -> Result<PasteReceipt> {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        ensure!(
            focused_window(&self.connection).await?.id == target.id,
            "Target window changed; transcription was not pasted"
        );
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        self.send_keys(keys).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if mode == DeliveryMode::Submit {
            let submitted: Result<()> = async {
                ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
                ensure!(
                    focused_window(&self.connection).await?.id == target.id,
                    "Target window changed before submission"
                );
                ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
                self.send_keys(&[28]).await?;
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            }
            .await;
            submitted.context("Text pasted; submission was not sent")?;
        }
        Ok(PasteReceipt {
            target: target.clone(),
            pane: None,
            ready_at: tokio::time::Instant::now(),
        })
    }
}

/// Proof of a successful paste, retaining its exact target for deferred Enter.
pub struct PasteReceipt {
    target: FocusTarget,
    pane: Option<String>,
    ready_at: tokio::time::Instant,
}

impl PasteReceipt {
    pub async fn submit(self, connection: &Connection, cancelled: &AtomicBool) -> Result<()> {
        let result = async {
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            tokio::time::sleep_until(self.ready_at).await;
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            if let Some(pane) = self.pane {
                crate::terminal::submit(connection, &self.target, &pane, cancelled).await
            } else {
                send_enter_to(connection, &self.target, cancelled).await
            }
        }
        .await;
        result.context("Text pasted; submission was not sent")
    }
}

/// A literal Enter at the current focus; no clipboard or dictation is involved.
pub async fn press_enter(connection: &Connection) -> Result<()> {
    let target = focused_window(connection).await?;
    send_enter_to(connection, &target, &AtomicBool::new(false)).await
}

pub async fn send_enter_to(
    connection: &Connection,
    target: &FocusTarget,
    cancelled: &AtomicBool,
) -> Result<()> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    let desktop = Desktop::connect(connection).await?;
    let result = async {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        ensure!(
            focused_window(connection).await?.id == target.id,
            "Target window changed before Enter"
        );
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        desktop.send_keys(&[28]).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        anyhow::Ok(())
    }
    .await;
    let stopped = desktop.stop().await;
    result?;
    stopped
}

#[derive(Debug, Clone, Deserialize)]
pub struct FocusTarget {
    pub id: u32,
    pub wm_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    focus: bool,
}

impl FocusTarget {
    pub fn is_terminal(&self) -> bool {
        let class = self.wm_class.to_lowercase();
        [
            "alacritty",
            "kitty",
            "terminator",
            "gnome-terminal",
            "konsole",
            "wezterm",
            "kgx",
            "org.gnome.console",
            "org.gnome.terminal",
            "xfce4-terminal",
            "mate-terminal",
            "tilix",
            "foot",
            "xterm",
            "urxvt",
            "terminology",
            "opencode",
        ]
        .iter()
        .any(|name| class.contains(name))
    }
}

pub async fn focused_window(connection: &Connection) -> Result<FocusTarget> {
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    let json: String = proxy.call("List", &()).await?;
    let windows: Vec<FocusTarget> = serde_json::from_str(&json)?;
    let mut target = windows
        .into_iter()
        .find(|window| window.focus)
        .context("No focused target window")?;
    // The Shell extension continuously tracks focus; read its current snapshot
    // rather than inferring the input target from the mouse position.
    if let Ok(context) = crate::context::snapshot(connection).await
        && context
            .application
            .trim_end_matches(".desktop")
            .eq_ignore_ascii_case(&target.wm_class)
    {
        target.title = context.window_title;
    }
    Ok(target)
}

async fn clipboard_read(primary: bool) -> Option<(String, Vec<u8>)> {
    let mut list = Command::new("wl-paste");
    if primary {
        list.arg("--primary");
    }
    let types = list.arg("--list-types").output().await.ok()?;
    if !types.status.success() {
        return None;
    }
    let types = String::from_utf8(types.stdout).ok()?;
    let mime = types
        .lines()
        .find(|name| name.starts_with("text/plain"))
        .or_else(|| types.lines().find(|name| *name == "image/png"))?
        .to_owned();
    let mut command = Command::new("wl-paste");
    if primary {
        command.arg("--primary");
    }
    let output = command
        .args(["--no-newline", "--type", &mime])
        .output()
        .await
        .ok()?;
    output.status.success().then_some((mime, output.stdout))
}

async fn restore_clipboard(primary: bool, snapshot: Option<(String, Vec<u8>)>) -> Result<()> {
    let mut command = Command::new("wl-copy");
    if primary {
        command.arg("--primary");
    }
    if let Some((mime, bytes)) = snapshot {
        let mut child = command
            .args(["--type", &mime])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("Clipboard stdin unavailable")?
            .write_all(&bytes)
            .await?;
        ensure!(child.wait().await?.success(), "Cannot restore clipboard");
    } else {
        command.arg("--clear").status().await?;
    }
    Ok(())
}

async fn stage_selection(
    primary: bool,
    text: &str,
    cancelled: &AtomicBool,
) -> Result<tokio::process::Child> {
    // Clipboard managers can take ownership while copying the same selection,
    // or briefly restore the previous contents. Retry a bounded number of times.
    for _ in 0..4 {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        let mut command = Command::new("wl-copy");
        command.args(["--foreground", "--type", "text/plain;charset=utf-8"]);
        if primary {
            command.arg("--primary");
        }
        let mut owner = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = owner.stdin.take().context("Clipboard stdin unavailable")?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        for _ in 0..50 {
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            if clipboard_read(primary)
                .await
                .is_some_and(|(_, bytes)| bytes == text.as_bytes())
            {
                return Ok(owner);
            }
            if owner.try_wait()?.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = owner.kill().await;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    anyhow::bail!("Cannot stage dictation text (primary: {primary})")
}

/// Stage Unicode text, verify focus again, paste, and restore the previous selection.
pub async fn paste_text(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_text_with_mode(connection, target, text, DeliveryMode::PasteOnly, cancelled).await
}

pub async fn paste_text_with_mode(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_with_receipt(connection, target, text, mode, cancelled)
        .await
        .map(|_| ())
}

pub async fn paste_text_with_receipt(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<PasteReceipt>> {
    paste_with_receipt(connection, target, text, DeliveryMode::PasteOnly, cancelled).await
}

async fn paste_with_receipt(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<PasteReceipt>> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    if text.trim().is_empty() {
        return Ok(None);
    }
    let current = focused_window(connection).await?;
    ensure!(
        current.id == target.id,
        "Target window changed; transcription was not pasted"
    );
    let terminal = if current.is_terminal() {
        Some(crate::terminal::resolve(&current).await)
    } else {
        None
    };
    if let Some(pane) = terminal
        .as_ref()
        .and_then(|context| context.pane.as_deref())
    {
        crate::terminal::paste(connection, &current, pane, text, mode, cancelled).await?;
        return Ok(Some(PasteReceipt {
            target: current.clone(),
            pane: Some(pane.to_owned()),
            ready_at: tokio::time::Instant::now() + Duration::from_millis(200),
        }));
    }
    let primary = terminal.as_ref().is_some_and(|context| context.agent);
    let keys: &[u32] = if primary {
        &[42, 110]
    } else if terminal.is_some() {
        &[29, 42, 47]
    } else {
        &[29, 47]
    };
    let previous = clipboard_read(primary).await;
    // Complete compositor setup before staging so other clipboard owners have
    // the smallest possible interval in which to replace the selection.
    let desktop = Desktop::connect(connection).await?;
    let mut owner = None;
    let result: Result<PasteReceipt> = async {
        owner = Some(stage_selection(primary, text, &cancelled).await?);
        desktop.paste_shortcut(keys, target, mode, &cancelled).await
    }
    .await;
    let stopped = desktop.stop().await;
    // Do not overwrite a newer clipboard selection made by the user meanwhile.
    if owner.is_some()
        && clipboard_read(primary)
            .await
            .is_some_and(|(_, bytes)| bytes == text.as_bytes())
        && let Err(error) = restore_clipboard(primary, previous).await
    {
        tracing::warn!(%error, "Clipboard restoration failed");
    }
    if let Some(mut owner) = owner {
        let _ = owner.kill().await;
    }
    let receipt = result?;
    stopped?;
    Ok(Some(receipt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_state_only_highlights_existing_first_five_targets() {
        assert_eq!(WorkspaceState { index: 4, count: 4 }.active_macro(), None);
        assert_eq!(
            WorkspaceState { index: 4, count: 5 }.active_macro(),
            Some(4)
        );
        assert_eq!(WorkspaceState { index: 5, count: 9 }.active_macro(), None);
        assert_eq!(
            WorkspaceState {
                index: -1,
                count: 5
            }
            .active_macro(),
            None
        );
    }

    #[test]
    fn bindings_require_the_exact_configured_accelerator() {
        assert!(binding_matches("['<Super>KP_1']\n", 0));
        assert!(binding_matches("['<Control>F5', '<Super>KP_5']", 4));
        assert!(!binding_matches("['<Super>KP_2']", 0));
        assert!(!binding_matches("['<Super>KP_10']", 0));
        assert!(!binding_matches("['<Shift><Super>KP_1']", 0));
        assert!(!binding_matches("@as []", 0));
        assert!(!binding_matches("['<Super>KP_6']", 5));
    }
}
