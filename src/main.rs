use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use anyhow::{Context, Result};
use clap::Parser;
use ksni::TrayMethods;
use sie::{
    browser,
    config::{AppConfig, DictationConfig},
    corrections::Database,
    desktop::{self, Desktop},
    dictation::{self, Command as DictationCommand, Phase, Status as DictationStatus},
    input,
    lighting::LightingMode,
    macros::InputAction,
    media::{self, Status as MediaStatus},
    media_focus, openrazer,
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
    /// Configuration path (default: XDG_CONFIG_HOME/sie/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Transcribe a 16 kHz mono 16-bit WAV locally and print corrected text; never paste.
    #[arg(long, conflicts_with_all = ["doctor", "apply_lighting", "switch_workspace", "tray_only"])]
    transcribe_file: Option<PathBuf>,
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
    let config = AppConfig::load(args.config)?;
    if let Some(path) = args.transcribe_file {
        let pcm = sie::speech::read_wav(&path)?;
        let engine = sie::speech::SpeechEngine::new(config.dictation.clone());
        let raw = engine.transcribe(&pcm, Arc::new(AtomicBool::new(false)))?;
        let processor = Database::connect(&config.dictation.database_url)
            .await?
            .postprocessor()
            .await?;
        println!("{}", processor.apply(&raw).0);
        return Ok(());
    }
    let connection = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
        .context("A desktop session D-Bus is required")?;
    if args.doctor {
        return doctor(&connection, &config.dictation).await;
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
    run(connection, args.tray_only, config.dictation).await
}

async fn doctor(connection: &Connection, dictation: &DictationConfig) -> Result<()> {
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
    if dictation.enabled {
        check!(
            "Whisper model",
            if dictation.model_path.is_file() {
                Ok(format!(
                    "local model; CUDA build: {}",
                    cfg!(feature = "cuda")
                ))
            } else {
                Err(anyhow::anyhow!("Local model is missing"))
            }
        );
        check!(
            "Desktop context",
            sie::context::snapshot(connection)
                .await
                .map(|context| context.application)
        );
        for (label, operation, device) in [
            (
                "Dictation microphone",
                "get-source-mute",
                dictation
                    .audio_source
                    .as_deref()
                    .unwrap_or("@DEFAULT_SOURCE@"),
            ),
            (
                "Dictation sound output",
                "get-sink-mute",
                dictation.sound_sink.as_deref().unwrap_or("@DEFAULT_SINK@"),
            ),
        ] {
            check!(
                label,
                async {
                    let output = tokio::process::Command::new("pactl")
                        .args([operation, device])
                        .env("LC_ALL", "C")
                        .output()
                        .await?;
                    anyhow::ensure!(
                        output.status.success(),
                        "Configured audio device is unavailable"
                    );
                    anyhow::Ok(format!(
                        "{} ({})",
                        device,
                        String::from_utf8_lossy(&output.stdout).trim()
                    ))
                }
                .await
            );
        }
        check!(
            "Dictation database",
            async {
                Database::connect(&dictation.database_url)
                    .await?
                    .postprocessor()
                    .await?;
                anyhow::Ok("independent correction database available")
            }
            .await
        );
    }
    anyhow::ensure!(
        failures.is_empty(),
        "Unavailable integrations: {}",
        failures.join(", ")
    );
    Ok(())
}

async fn run(
    connection: Connection,
    tray_only: bool,
    dictation_config: DictationConfig,
) -> Result<()> {
    let mut lighting_mode = LightingMode::On;
    let broker = Arc::new(browser::Broker::default());
    browser::export(&connection, broker.clone()).await?;
    let (focus_requests, focus_rx) = mpsc::channel(8);
    let (native_hints, native_hint_rx) = tokio::sync::watch::channel(None);
    let (focus_status, mut focus_status_rx) = tokio::sync::watch::channel(None);
    let initial = DictationStatus::new(
        dictation_config.enabled && !tray_only,
        if dictation_config.enabled && !tray_only {
            Phase::Idle
        } else {
            Phase::Disabled
        },
    );
    let (dictation_commands, dictation_rx) = mpsc::channel(16);
    let (dictation_status, mut dictation_status_rx) = tokio::sync::watch::channel(initial.clone());
    let (actions, mut action_rx) = mpsc::channel(8);
    let (media_status, mut media_rx) = tokio::sync::watch::channel(MediaStatus::default());
    let (media_targets, media_target_rx) = mpsc::channel(8);
    let tray = AppTray {
        actions,
        lighting_status: if tray_only {
            "Řízení osvětlení vypnuté"
        } else {
            "Připojuji klávesnici…"
        }
        .into(),
        lighting_mode,
        media_status: MediaStatus::default().label(),
        focus_status: None,
        lighting_controls_available: !tray_only,
        dictation_status: initial,
        dictation_controls_available: !tray_only,
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
    let mut dictation_task = None;
    if !tray_only {
        tasks.spawn(input::monitor(macros));
        tasks.spawn(media_focus::run(
            connection.clone(),
            broker,
            focus_rx,
            native_hint_rx,
            focus_status,
        ));
        tasks.spawn(media::watch(
            connection.clone(),
            media_status,
            media_target_rx,
        ));
        // A separate, optional read-only observer cannot interfere with the
        // normal D-Bus connection used by the application.
        match zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(1))
            .build()
            .await
        {
            Ok(observer) => {
                tasks.spawn(async move {
                    let result = async {
                        media::NativeObserver::new(observer)
                            .await?
                            .run_with_hint(media_targets, Some(native_hints))
                            .await
                    }
                    .await;
                    if let Err(error) = result {
                        tracing::debug!(%error,"Native media target observer unavailable; using MPRIS events");
                    }
                });
            }
            Err(error) => tracing::debug!(%error,"Native media target observer unavailable"),
        }
        dictation_task = Some(tokio::spawn(dictation::run(
            dictation_config,
            connection.clone(),
            dictation_rx,
            dictation_status,
        )));
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
                    let media=media_rx.borrow_and_update().clone();
                    refresh_lighting(&connection, &tray, lighting_mode, &media, &mut last_status).await;
                },
                Some(Action::Dictation(command)) => if !tray_only { let _ = dictation_commands.try_send(command); },
                Some(Action::About) => if about.is_empty() {
                    about.spawn(async { if let Err(error) = sie::tray::show_about().await { tracing::warn!(%error, "About dialog failed"); } });
                },
            },
            Some(_) = about.join_next(), if !about.is_empty() => {},
            Some(action) = macro_rx.recv(), if !tray_only => {
                let index = match action {
                    InputAction::Workspace(index) => index,
                    InputAction::MediaReveal => {
                        if let Some(fallback) = media_rx.borrow().target.clone() {
                            let _ = focus_requests.try_send(media_focus::Request {
                                at: std::time::Instant::now(),
                                fallback,
                            });
                        }
                        continue;
                    },
                    InputAction::DictationToggle => { let _ = dictation_commands.try_send(DictationCommand::Toggle); continue; },
                    InputAction::DictationCancel => { let _ = dictation_commands.try_send(DictationCommand::Cancel); continue; },
                };
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
            Ok(()) = focus_status_rx.changed(), if !tray_only => {
                let status = focus_status_rx.borrow_and_update().clone();
                tray.update(move |tray| tray.focus_status = status).await;
            },
            Ok(()) = dictation_status_rx.changed(), if !tray_only => {
                let status=dictation_status_rx.borrow_and_update().clone();
                tracing::info!(phase=?status.phase,"Dictation state changed");
                tray.update(move |tray| tray.dictation_status=status).await;
            },
            _ = async {
                tokio::select! {
                    _ = refresh.tick() => {},
                    Some(()) = workspace_rx.recv() => {},
                    Ok(()) = media_rx.changed() => {},
                }
            }, if !tray_only => {
                let media=media_rx.borrow_and_update().clone();
                refresh_lighting(&connection, &tray, lighting_mode, &media, &mut last_status).await;
            },
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::error!(%error, "Input monitor stopped"); }
            },
        }
    }
    drop(dictation_commands);
    if let Some(mut task) = dictation_task
        && tokio::time::timeout(Duration::from_secs(4), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
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
    media: &MediaStatus,
    last_status: &mut String,
) {
    let state = desktop::workspace_state(connection).await.ok();
    let active = state.and_then(|state| state.active_macro());
    let lighting = match openrazer::apply_status(connection, active, mode, media.playback).await {
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
    let media = media.label();
    let status = format!("{mode:?}: {lighting}; {workspace}; {media}");
    if status != *last_status {
        tracing::info!("{status}");
        *last_status = status;
        tray.update(move |tray| {
            tray.lighting_status = lighting;
            tray.lighting_mode = mode;
            tray.workspace_status = workspace;
            tray.media_status = media;
        })
        .await;
    }
}
