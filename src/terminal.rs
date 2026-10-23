//! Resolve only the focused terminal's tmux client; never pick an unrelated pane.
use crate::desktop::FocusTarget;
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
        if let Ok(children) =
            tokio::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).await
        {
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
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_checked(None, pane, text, cancelled, async {
        let current = crate::desktop::focused_window(connection).await?;
        ensure!(
            current.id == target.id && resolve(&current).await.pane.as_deref() == Some(pane),
            "Target window changed; transcription was not pasted"
        );
        Ok(())
    })
    .await
}

async fn paste_checked(
    socket: Option<&Path>,
    pane: &str,
    text: &str,
    cancelled: Arc<AtomicBool>,
    verify: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
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
        verify.await?;
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        // -p preserves bracketed paste; -d removes only our private buffer.
        // No keypress or Enter is generated, and the shared clipboard is untouched.
        query_on(
            socket,
            &["paste-buffer", "-p", "-d", "-b", &name, "-t", pane],
        )
        .await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = query_on(socket, &["delete-buffer", "-b", &name]).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    struct PrivateTmux {
        directory: tempfile::TempDir,
    }
    impl PrivateTmux {
        fn socket(&self) -> std::path::PathBuf {
            self.directory.path().join("socket")
        }
    }
    impl Drop for PrivateTmux {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .arg("-S")
                .arg(self.socket())
                .arg("kill-server")
                .output();
        }
    }

    #[tokio::test]
    async fn private_tmux_delivers_unicode_without_enter_and_suppresses_canceled_or_refocused_output()
     {
        let server = PrivateTmux {
            directory: tempfile::tempdir().unwrap(),
        };
        let socket = server.socket();
        let socket = Some(socket.as_path());
        query_on(
            socket,
            &["-f", "/dev/null", "new-session", "-d", "-s", "probe", "cat"],
        )
        .await
        .unwrap();
        query_on(socket, &["new-window", "-d", "-t", "probe", "cat"])
            .await
            .unwrap();
        query_on(socket, &["copy-mode", "-t", "%0"]).await.unwrap();
        query_on(socket, &["copy-mode", "-t", "%1"]).await.unwrap();
        let phrase = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
        paste_checked(
            socket,
            "%0",
            phrase,
            Arc::new(AtomicBool::new(false)),
            async { Ok(()) },
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let captured = query_on(socket, &["capture-pane", "-p", "-t", "%0"])
            .await
            .unwrap();
        assert!(captured.contains(phrase));
        // cat's canonical input has echoed the phrase but received no Enter.
        assert_eq!(captured.trim_end(), phrase);
        assert_eq!(
            query_on(
                socket,
                &["display-message", "-p", "-t", "%0", "#{pane_in_mode}"]
            )
            .await
            .unwrap()
            .trim(),
            "0"
        );
        assert_eq!(
            query_on(
                socket,
                &["display-message", "-p", "-t", "%1", "#{pane_in_mode}"]
            )
            .await
            .unwrap()
            .trim(),
            "1"
        );
        assert!(
            !query_on(socket, &["capture-pane", "-p", "-t", "%1"])
                .await
                .unwrap()
                .contains(phrase)
        );
        assert!(
            paste_checked(
                socket,
                "%0",
                "CANCELED",
                Arc::new(AtomicBool::new(true)),
                async { Ok(()) }
            )
            .await
            .is_err()
        );
        assert!(
            paste_checked(
                socket,
                "%0",
                "WRONG TARGET",
                Arc::new(AtomicBool::new(false)),
                async { anyhow::bail!("focus changed") }
            )
            .await
            .is_err()
        );
        let after = query_on(socket, &["capture-pane", "-p", "-t", "%0"])
            .await
            .unwrap();
        assert_eq!(after, captured);
        assert!(
            !query_on(socket, &["list-buffers", "-F", "#{buffer_name}"])
                .await
                .unwrap()
                .contains("sie-dictation-")
        );
    }

    #[test]
    fn client_resolution_rejects_unrelated_ambiguous_and_malformed_targets() {
        let pids = HashSet::from([100, 101]);
        assert_eq!(matching_pane("99\t%1\n100\t%2\n", &pids), Some("%2".into()));
        assert_eq!(matching_pane("100\t%2\n101\t%3\n", &pids), None);
        assert_eq!(matching_pane("100\tsession:0\ninvalid\t%2\n", &pids), None);
        assert_eq!(matching_pane("99\t%1\n", &pids), None);
        assert_eq!(
            matching_pane("100\t%2\n101\t%2\n", &pids),
            Some("%2".into())
        );
    }
    #[test]
    fn agent_titles_and_commands_are_distinct_from_plain_shells() {
        for name in ["codex-sie", "Claude Code", "opencode", "Gemini CLI"] {
            assert!(agent_name(name));
        }
        for name in ["bash", "python3", "gnome-terminal"] {
            assert!(!agent_name(name));
        }
    }
}
