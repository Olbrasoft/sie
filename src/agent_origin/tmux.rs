//! Parse and query tmux routing metadata without inspecting pane contents.
use super::{Client, Pane};
use anyhow::{Result, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

fn numbered(value: &str, prefix: char) -> bool {
    value.starts_with(prefix) && value.len() > 1 && value[1..].bytes().all(|b| b.is_ascii_digit())
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
