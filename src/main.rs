use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ksni::TrayMethods;
use sie::{
    desktop::{self, Desktop},
    input,
    lighting::LightingMode,
    openrazer,
    tray::{Action, AppTray},
};
use tokio::{sync::mpsc, task::JoinSet};
use zbus::Connection;

#[derive(Parser)]
#[command(
    version,
    about = "System Interface Extensions — GNOME/Wayland keyboard proof of concept"
)]
struct Args {
    /// Check the existing desktop integrations without changing settings or lighting.
    #[arg(long, conflicts_with_all = ["apply_lighting", "switch_workspace", "tray_only"])]
    doctor: bool,
    /// Apply the native touch-typing frame once, then exit.
    #[arg(long, conflicts_with_all = ["switch_workspace", "tray_only"])]
    apply_lighting: bool,
    /// Test a zero-based workspace target (0–4), then exit.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=4), conflicts_with = "tray_only")]
    switch_workspace: Option<u32>,
    /// Run the tray without taking control of keyboard lighting or macro keys.
    #[arg(long)]
    tray_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sie=info".into()),
        )
        .init();
    let connection = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
        .context("A desktop session D-Bus is required")?;
    if args.doctor {
        return doctor(&connection).await;
    }

    // DoNotQueue prevents a second instance from waiting and later taking ownership.
    sie::claim_instance(&connection)
        .await
        .context("SIE is already running, or its application name cannot be acquired")?;
    if args.apply_lighting {
        let active = desktop::workspace_state(&connection)
            .await
            .ok()
            .and_then(|state| state.active_macro());
        let count = openrazer::apply(&connection, active).await?;
        anyhow::ensure!(count > 0, "No supported BlackWidow V4 Pro found");
        println!("Touch-typing lighting applied to {count} keyboard(s).");
        return Ok(());
    }
    if let Some(index) = args.switch_workspace {
        let desktop = Desktop::connect(&connection).await?;
        let result = desktop.switch(index).await;
        desktop.stop().await?;
        println!(
            "Workspace {index}: {}",
            if result? {
                "shortcut sent"
            } else {
                "unavailable; ignored"
            }
        );
        return Ok(());
    }
    run(connection, args.tray_only).await
}

async fn doctor(connection: &Connection) -> Result<()> {
    let mut failures = Vec::new();
    macro_rules! check {
        ($name:expr, $result:expr) => {
            match $result {
                Ok(detail) => println!("OK   {}: {detail}", $name),
                Err(error) => {
                    println!("FAIL {}: {error:#}", $name);
                    failures.push($name);
                }
            }
        };
    }
    check!(
        "OpenRazer",
        openrazer::devices(connection).await.and_then(|paths| {
            anyhow::ensure!(!paths.is_empty(), "No supported keyboard found");
            Ok(format!("{} BlackWidow V4 Pro, matrix 8 × 23", paths.len()))
        })
    );
    check!(
        "Macro input",
        input::discover().and_then(|devices| {
            anyhow::ensure!(
                !devices.is_empty(),
                "No readable supported keyboard input; check /dev/input permissions"
            );
            Ok(format!("{} interface(s)", devices.len()))
        })
    );
    check!(
        "Workspace tracker",
        desktop::workspace_count(connection)
            .await
            .map(|n| format!("{n} existing workspaces"))
    );
    for index in 0..5 {
        check!(
            "Workspace shortcut",
            desktop::check_binding(index)
                .await
                .map(|()| format!("M{} → workspace {index}", index + 1))
        );
    }
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    check!(
        "Tray host",
        dbus.name_has_owner("org.kde.StatusNotifierWatcher".try_into()?)
            .await
            .and_then(|owned| {
                if owned {
                    Ok("StatusNotifierWatcher available")
                } else {
                    Err(zbus::fdo::Error::Failed(
                        "Enable GNOME AppIndicator support".into(),
                    ))
                }
            })
    );
    check!(
        "Mutter input",
        zbus::Proxy::new(
            connection,
            "org.gnome.Mutter.RemoteDesktop",
            "/org/gnome/Mutter/RemoteDesktop",
            "org.gnome.Mutter.RemoteDesktop"
        )
        .await?
        .get_property::<i32>("Version")
        .await
        .map(|v| format!("API version {v}"))
    );
    check!(
        "About dialog",
        tokio::process::Command::new("zenity")
            .arg("--version")
            .output()
            .await
            .and_then(|out| {
                if out.status.success() {
                    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
                } else {
                    Err(std::io::Error::other("zenity unavailable"))
                }
            })
    );
    anyhow::ensure!(
        failures.is_empty(),
        "Unavailable integrations: {}",
        failures.join(", ")
    );
    Ok(())
}

async fn run(connection: Connection, tray_only: bool) -> Result<()> {
    let mut lighting_mode = LightingMode::On;
    let (actions, mut action_rx) = mpsc::channel(8);
    let tray = AppTray {
        actions,
        lighting_status: if tray_only {
            "Řízení osvětlení vypnuté"
        } else {
            "Připojuji klávesnici…"
        }
        .into(),
        lighting_mode,
        lighting_controls_available: !tray_only,
        workspace_status: if tray_only {
            "Makroklávesy vypnuty"
        } else {
            "Kontroluji ovládání ploch…"
        }
        .into(),
    }
    .spawn()
    .await
    .context("Cannot display tray; GNOME requires AppIndicator support")?;
    let mut tasks = JoinSet::new();
    let mut about = JoinSet::new();
    let (macros, mut macro_rx) = mpsc::channel(8);
    let (workspace_changes, mut workspace_rx) = mpsc::channel(1);
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if !tray_only {
        tasks.spawn(input::monitor(macros));
        tasks.spawn(desktop::watch_workspaces(
            connection.clone(),
            workspace_changes,
        ));
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut last_status = String::new();
    tracing::info!(version = sie::VERSION, "SIE started");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
            action = action_rx.recv() => match action {
                Some(Action::Quit) | None => break,
                Some(Action::SetLightingMode(mode)) => if !tray_only {
                    lighting_mode = mode;
                    refresh_lighting(&connection, &tray, lighting_mode, &mut last_status).await;
                },
                Some(Action::About) => if about.is_empty() {
                    about.spawn(async { if let Err(error) = sie::tray::show_about().await { tracing::warn!(%error, "About dialog failed"); } });
                },
            },
            Some(_) = about.join_next(), if !about.is_empty() => {},
            Some(index) = macro_rx.recv(), if !tray_only => {
                // Keep each remote-input session short and release it after the shortcut.
                let result = async {
                    let desktop = Desktop::connect(&connection).await?;
                    let result = desktop.switch(index).await;
                    let stopped = desktop.stop().await;
                    let changed = result?;
                    stopped?;
                    anyhow::Ok(changed)
                }.await;
                match result {
                    Ok(true) => tracing::info!(workspace = index, "Workspace shortcut sent"),
                    Ok(false) => tracing::debug!(workspace = index, "Unavailable workspace ignored"),
                    Err(error) => tracing::warn!("Workspace switch failed: {error:#}"),
                }
            },
            _ = async {
                tokio::select! {
                    _ = refresh.tick() => {},
                    Some(()) = workspace_rx.recv() => {},
                }
            }, if !tray_only => {
                refresh_lighting(&connection, &tray, lighting_mode, &mut last_status).await;
            },
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::error!(%error, "Input monitor stopped"); }
            },
        }
    }
    tasks.abort_all();
    about.abort_all();
    while tasks.join_next().await.is_some() {}
    while about.join_next().await.is_some() {}
    tray.shutdown().await;
    tracing::info!("SIE stopped");
    Ok(())
}

async fn refresh_lighting(
    connection: &Connection,
    tray: &ksni::Handle<AppTray>,
    mode: LightingMode,
    last_status: &mut String,
) {
    let state = desktop::workspace_state(connection).await.ok();
    let active = state.and_then(|state| state.active_macro());
    let lighting = match openrazer::apply_mode(connection, active, mode).await {
        Ok(0) => "Klávesnice není připojena".to_owned(),
        Ok(_) => match mode {
            LightingMode::On => "Podsvícení aktivní",
            LightingMode::Off => "Osvětlení vypnuté",
        }
        .to_owned(),
        Err(error) => {
            tracing::warn!("Lighting unavailable: {error:#}");
            "Podsvícení nedostupné".to_owned()
        }
    };
    let workspace = state
        .map(sie::tray::workspace_label)
        .unwrap_or_else(|| "Ovládání ploch nedostupné".into());
    let status = format!("{mode:?}: {lighting}; {workspace}");
    if status != *last_status {
        tracing::info!("{status}");
        *last_status = status;
        tray.update(move |tray| {
            tray.lighting_status = lighting;
            tray.lighting_mode = mode;
            tray.workspace_status = workspace;
        })
        .await;
    }
}
