//! Route question metadata to one live terminal, without inspecting terminal text.
use crate::switcher::Window;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use zbus::Connection;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Question {
    pub agent: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub text: String,
}

impl Question {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.agent.as_str(),
                "codex" | "claude" | "opencode" | "gemini"
            ),
            "Unsupported agent"
        );
        ensure!(
            self.session_id.len() <= 128
                && !self.session_id.is_empty()
                && self
                    .session_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid session identity"
        );
        ensure!(
            self.cwd.is_absolute() && self.cwd.as_os_str().len() <= 4096,
            "Invalid agent directory"
        );
        ensure!(
            !self.text.trim().is_empty() && self.text.len() <= 16384,
            "Invalid question text"
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub started: u64,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub daemon: bool,
}

#[derive(Debug, Clone)]
pub struct Pane {
    pub id: String,
    pub pid: u32,
    pub session: String,
    pub window: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Client {
    pub pid: u32,
    pub tty: String,
    pub session: String,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub processes: HashMap<u32, Process>,
    pub panes: Vec<Pane>,
    pub clients: Vec<Client>,
    pub windows: Vec<Window>,
    pub indexed_pids: HashSet<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub window: u32,
    pub window_pid: u32,
    pub window_started: u64,
    pub wm_class: String,
    pub agent_pid: u32,
    pub agent_started: u64,
    pub pane: String,
    pub pane_pid: u32,
    pub pane_started: u64,
    pub session: String,
    pub tmux_window: String,
    pub client_pid: u32,
    pub client_started: u64,
    pub client_tty: String,
}

fn descendant(processes: &HashMap<u32, Process>, mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        let Some(process) = processes.get(&pid) else {
            return false;
        };
        if process.parent <= 1 || process.parent == pid {
            return false;
        }
        pid = process.parent;
    }
    false
}

fn terminal(class: &str) -> bool {
    let class = class.to_lowercase();
    [
        "alacritty",
        "kitty",
        "gnome-terminal",
        "org.gnome.terminal",
        "terminator",
        "org.gnome.console",
        "konsole",
        "com.mitchellh.ghostty",
    ]
    .iter()
    .any(|name| class == *name || class.starts_with(&format!("{name}.")))
}

fn numbered(value: &str, prefix: char) -> bool {
    value.starts_with(prefix) && value.len() > 1 && value[1..].bytes().all(|b| b.is_ascii_digit())
}

pub fn resolve(snapshot: &Snapshot, question: &Question) -> Option<Origin> {
    let agents: Vec<_> = snapshot
        .processes
        .values()
        .filter(|p| p.name == question.agent && !p.daemon && p.cwd.as_ref() == Some(&question.cwd))
        .collect();
    let indexed: Vec<_> = agents
        .iter()
        .filter(|p| snapshot.indexed_pids.contains(&p.pid))
        .copied()
        .collect();
    let agents = if indexed.is_empty() { agents } else { indexed };
    let mut found = Vec::new();
    for agent in agents {
        for pane in &snapshot.panes {
            if pane.cwd != question.cwd || !descendant(&snapshot.processes, agent.pid, pane.pid) {
                continue;
            }
            let Some(pane_process) = snapshot.processes.get(&pane.pid) else {
                continue;
            };
            for client in snapshot
                .clients
                .iter()
                .filter(|c| c.session == pane.session)
            {
                let Some(client_process) = snapshot.processes.get(&client.pid) else {
                    continue;
                };
                for window in snapshot.windows.iter().filter(|w| {
                    w.id > 0
                        && w.workspace.is_some_and(|i| i >= 0)
                        && matches!(w.window_type, 0 | 3 | 4)
                        && terminal(&w.wm_class)
                }) {
                    if !descendant(&snapshot.processes, client.pid, window.pid) {
                        continue;
                    }
                    let Some(gui) = snapshot.processes.get(&window.pid) else {
                        continue;
                    };
                    found.push(Origin {
                        window: window.id,
                        window_pid: gui.pid,
                        window_started: gui.started,
                        wm_class: window.wm_class.clone(),
                        agent_pid: agent.pid,
                        agent_started: agent.started,
                        pane: pane.id.clone(),
                        pane_pid: pane.pid,
                        pane_started: pane_process.started,
                        session: pane.session.clone(),
                        tmux_window: pane.window.clone(),
                        client_pid: client.pid,
                        client_started: client_process.started,
                        client_tty: client.tty.clone(),
                    });
                }
            }
        }
    }
    found.dedup();
    (found.len() == 1).then(|| found.remove(0))
}

impl Origin {
    pub fn still_valid(&self, snapshot: &Snapshot, question: &Question) -> bool {
        for (pid, start) in [
            (self.window_pid, self.window_started),
            (self.agent_pid, self.agent_started),
            (self.pane_pid, self.pane_started),
            (self.client_pid, self.client_started),
        ] {
            if snapshot
                .processes
                .get(&pid)
                .is_none_or(|p| p.started != start)
            {
                return false;
            }
        }
        snapshot.windows.iter().any(|w| {
            w.id == self.window
                && w.pid == self.window_pid
                && w.wm_class == self.wm_class
                && w.workspace.is_some_and(|i| i >= 0)
        }) && snapshot.panes.iter().any(|p| {
            p.id == self.pane
                && p.pid == self.pane_pid
                && p.session == self.session
                && p.window == self.tmux_window
                && p.cwd == question.cwd
        }) && snapshot
            .clients
            .iter()
            .any(|c| c.pid == self.client_pid && c.tty == self.client_tty)
            && snapshot.processes.get(&self.agent_pid).is_some_and(|p| {
                p.name == question.agent && !p.daemon && p.cwd.as_ref() == Some(&question.cwd)
            })
            && descendant(&snapshot.processes, self.agent_pid, self.pane_pid)
            && descendant(&snapshot.processes, self.client_pid, self.window_pid)
    }

    pub fn workspace(&self, snapshot: &Snapshot) -> Option<i32> {
        snapshot
            .windows
            .iter()
            .find(|w| w.id == self.window)
            .and_then(|w| w.workspace)
    }
}

fn parse_process(pid: u32, stat: &str, name: String) -> Option<Process> {
    let (_, tail) = stat.rsplit_once(')')?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    Some(Process {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        started: fields.get(19)?.parse().ok()?,
        name,
        cwd: None,
        daemon: false,
    })
}

fn processes() -> HashMap<u32, Process> {
    use std::os::unix::fs::MetadataExt;
    let mut result = HashMap::new();
    let Ok(uid) = std::fs::metadata("/proc/self").map(|m| m.uid()) else {
        return result;
    };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return result;
    };
    for entry in entries.flatten().take(16384) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !entry.metadata().is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let path = entry.path();
        let Ok(stat) = std::fs::read_to_string(path.join("stat")) else {
            continue;
        };
        let Ok(name) = std::fs::read_to_string(path.join("comm")) else {
            continue;
        };
        let Some(mut process) = parse_process(pid, &stat, name.trim().into()) else {
            continue;
        };
        if matches!(
            process.name.as_str(),
            "codex" | "claude" | "opencode" | "gemini"
        ) {
            process.cwd = std::fs::read_link(path.join("cwd")).ok();
            // Exclude shared app-server daemons: their PID is not a TUI origin.
            if let Ok(args) = std::fs::read(path.join("cmdline")) {
                process.daemon = args
                    .split(|b| *b == 0)
                    .any(|a| a == b"app-server" || a == b"daemon");
            }
        }
        result.insert(pid, process);
    }
    result
}

pub fn parse_panes(value: &str) -> Vec<Pane> {
    value
        .lines()
        .filter_map(|line| {
            let values: Vec<_> = line.split('\t').collect();
            if values.len() != 5
                || !numbered(values[0], '%')
                || !numbered(values[3], '@')
                || !Path::new(values[4]).is_absolute()
            {
                return None;
            }
            Some(Pane {
                id: values[0].into(),
                pid: values[1].parse().ok()?,
                session: values[2].into(),
                window: values[3].into(),
                cwd: values[4].into(),
            })
        })
        .collect()
}

pub fn parse_clients(value: &str) -> Vec<Client> {
    value
        .lines()
        .filter_map(|line| {
            let values: Vec<_> = line.split('\t').collect();
            if values.len() != 3 || !values[1].starts_with("/dev/") {
                return None;
            }
            Some(Client {
                pid: values[0].parse().ok()?,
                tty: values[1].into(),
                session: values[2].into(),
            })
        })
        .collect()
}

pub async fn tmux(args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(1),
        Command::new("tmux")
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "Cannot query/select existing agent pane"
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn indexed_pids(question: &Question) -> HashSet<u32> {
    let mut pids = HashSet::new();
    let Some(home) = std::env::var_os("HOME") else {
        return pids;
    };
    let root = if question.agent == "codex" {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".codex"))
            .join("sessions/pids")
    } else {
        PathBuf::from(home).join(format!(".{}/sessions", question.agent))
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return pids;
    };
    for entry in entries.flatten().take(1024) {
        if !entry
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= 4096)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value.get("sessionId").and_then(|v| v.as_str()) == Some(&question.session_id)
            && let Some(pid) = value
                .get("pid")
                .and_then(|v| v.as_u64())
                .and_then(|p| u32::try_from(p).ok())
        {
            pids.insert(pid);
        }
    }
    pids
}

pub async fn snapshot(connection: &Connection, question: &Question) -> Result<Snapshot> {
    let windows = crate::switcher::windows(connection).await?;
    let panes = parse_panes(
        &tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{session_name}\t#{window_id}\t#{pane_current_path}",
        ])
        .await?,
    );
    let clients = parse_clients(
        &tmux(&[
            "list-clients",
            "-F",
            "#{client_pid}\t#{client_tty}\t#{session_name}",
        ])
        .await?,
    );
    Ok(Snapshot {
        windows,
        panes,
        clients,
        processes: processes(),
        indexed_pids: indexed_pids(question),
    })
}

pub async fn activate(connection: &Connection, origin: &Origin, question: &Question) -> Result<()> {
    let current = snapshot(connection, question).await?;
    ensure!(
        origin.still_valid(&current, question),
        "Question terminal has closed or changed identity"
    );
    crate::switcher::activate(connection, origin.window, None).await?;
    let current = snapshot(connection, question).await?;
    ensure!(
        origin.still_valid(&current, question),
        "Question target changed during activation"
    );
    tmux(&[
        "switch-client",
        "-c",
        &origin.client_tty,
        "-t",
        &format!("={}", origin.session),
    ])
    .await?;
    tmux(&[
        "select-window",
        "-t",
        &format!("={}:{}", origin.session, origin.tmux_window),
    ])
    .await?;
    tmux(&["select-pane", "-t", &origin.pane]).await?;
    Ok(())
}
