//! Assemble live snapshots and activate only a revalidated origin.
use super::{
    Origin, Question, Snapshot, parse_clients, parse_panes,
    processes::{indexed_pids, processes},
    tmux,
};
use anyhow::{Result, ensure};
use zbus::Connection;

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
