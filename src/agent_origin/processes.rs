//! Bounded process metadata and agent-session PID indexes; never terminal text.
use super::{Process, Question};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

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

pub(super) fn processes() -> HashMap<u32, Process> {
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

pub(super) fn indexed_pids(question: &Question) -> HashSet<u32> {
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
