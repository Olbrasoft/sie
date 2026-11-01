//! Validated question metadata and process/window snapshots.
use crate::switcher::Window;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

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
