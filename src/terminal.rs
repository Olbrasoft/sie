//! Resolve only the focused terminal's tmux client; never pick an unrelated pane.
use crate::desktop::{DeliveryMode, FocusTarget};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Debug, Clone)]
pub struct TerminalContext {
    pub pane: Option<String>,
    pub agent: bool,
}

pub fn matching_pane(clients: &str, descendants: &HashSet<u32>) -> Option<String> {
    let mut panes = HashSet::new();
    for line in clients.lines() {
        let Some((pid, pane)) = line.split_once('\t') else {
            continue;
        };
        if pid
            .parse::<u32>()
            .ok()
            .is_some_and(|pid| descendants.contains(&pid))
            && pane.starts_with('%')
            && pane.len() > 1
            && pane[1..].bytes().all(|b| b.is_ascii_digit())
        {
            panes.insert(pane.to_owned());
        }
    }
    (panes.len() == 1).then(|| panes.into_iter().next().unwrap())
}

pub fn agent_name(value: &str) -> bool {
    let value = value.to_lowercase();
    ["claude", "codex", "opencode", "gemini"]
        .iter()
        .any(|name| value.contains(name))
}

async fn descendants(pid: u32) -> HashSet<u32> {
    let mut seen = HashSet::new();
    if pid == 0 {
        return seen;
    }
    let mut pending = vec![pid];
    seen.insert(pid);
    while let Some(pid) = pending.pop() {
        if seen.len() > 512 {
            break;
        }
        // A GUI may launch its client from a worker thread. Linux children
        // files are per-thread, so inspect every task rather than only the leader.
        let Ok(mut tasks) = tokio::fs::read_dir(format!("/proc/{pid}/task")).await else {
            continue;
        };
        while let Ok(Some(task)) = tasks.next_entry().await {
            let Ok(children) = tokio::fs::read_to_string(task.path().join("children")).await else {
                continue;
            };
            for child in children
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
            {
                if seen.insert(child) {
                    pending.push(child);
                }
            }
        }
    }
    seen
}

fn tmux_command(socket: Option<&Path>) -> Command {
    let mut command = Command::new("tmux");
    if let Some(socket) = socket {
        command.arg("-S").arg(socket);
    }
    command
}

async fn query(args: &[&str]) -> Result<String> {
    query_on(None, args).await
}

async fn query_on(socket: Option<&Path>, args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        tmux_command(socket)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(output.status.success(), "Cannot query focused tmux pane");
    Ok(String::from_utf8(output.stdout)?)
}

pub async fn resolve(target: &FocusTarget) -> TerminalContext {
    let descendants = descendants(target.pid).await;
    let pane = match query(&["list-clients", "-F", "#{client_pid}\t#{pane_id}"]).await {
        Ok(output) => matching_pane(&output, &descendants),
        Err(_) => None,
    };
    let mut agent = agent_name(&target.title);
    if !agent {
        for pid in descendants {
            if let Ok(name) = tokio::fs::read_to_string(format!("/proc/{pid}/comm")).await
                && agent_name(&name)
            {
                agent = true;
                break;
            }
        }
    }
    TerminalContext { pane, agent }
}

pub async fn paste(
    connection: &zbus::Connection,
    target: &FocusTarget,
    pane: &str,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_checked(None, pane, text, mode, cancelled, || {
        verify_target(connection, target, pane)
    })
    .await
}

async fn verify_target(
    connection: &zbus::Connection,
    target: &FocusTarget,
    pane: &str,
) -> Result<()> {
    let current = crate::desktop::focused_window(connection).await?;
    ensure!(
        current.id == target.id && resolve(&current).await.pane.as_deref() == Some(pane),
        "Target window changed or terminal pane changed"
    );
    Ok(())
}

pub async fn submit(
    connection: &zbus::Connection,
    target: &FocusTarget,
    pane: &str,
    cancelled: &AtomicBool,
) -> Result<()> {
    submit_checked(None, pane, cancelled, || {
        verify_target(connection, target, pane)
    })
    .await
}

async fn submit_checked<F: std::future::Future<Output = Result<()>>>(
    socket: Option<&Path>,
    pane: &str,
    cancelled: &AtomicBool,
    mut verify: impl FnMut() -> F,
) -> Result<()> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    verify().await?;
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    query_on(socket, &["send-keys", "-t", pane, "Enter"]).await?;
    Ok(())
}

async fn paste_checked<F: std::future::Future<Output = Result<()>>>(
    socket: Option<&Path>,
    pane: &str,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
    mut verify: impl FnMut() -> F,
) -> Result<()> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    if text.trim().is_empty() {
        return Ok(());
    }
    if query_on(
        socket,
        &["display-message", "-p", "-t", pane, "#{pane_in_mode}"],
    )
    .await?
    .trim()
        == "1"
    {
        query_on(socket, &["send-keys", "-X", "-t", pane, "cancel"]).await?;
    }
    static NEXT_BUFFER: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "sie-dictation-{}-{}",
        std::process::id(),
        NEXT_BUFFER.fetch_add(1, Ordering::Relaxed)
    );
    let result: Result<()> = async {
        let mut child = tmux_command(socket)
            .args(["load-buffer", "-b", &name, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().context("tmux input unavailable")?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        ensure!(
            tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await??
                .success(),
            "Cannot stage tmux dictation"
        );
        verify().await?;
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        // -p preserves bracketed paste; -d removes only our private buffer.
        // The shared clipboard is untouched; submission is a separate key.
        query_on(
            socket,
            &["paste-buffer", "-p", "-d", "-b", &name, "-t", pane],
        )
        .await?;
        if mode == DeliveryMode::Submit {
            // Let terminal UIs finish their bracketed-paste event before Enter.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let submitted = submit_checked(socket, pane, &cancelled, &mut verify).await;
            submitted.context("Text pasted; submission was not sent")?;
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = query_on(socket, &["delete-buffer", "-b", &name]).await;
    }
    result
}

#[cfg(test)]
mod tests;
