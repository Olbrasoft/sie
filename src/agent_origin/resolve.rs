//! Pure origin selection and revalidation; no process or desktop I/O.
use super::{Origin, Process, Question, Snapshot};
use std::collections::HashMap;

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
