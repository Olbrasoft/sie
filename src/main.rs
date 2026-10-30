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
    lighting::{LightingMode, State as LightingState},
    macros::InputAction,
    media::{self, Status as MediaStatus},
    media_focus, mouse_lighting, openrazer,
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
    run(connection, args.tray_only, config).await
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
        "Mouse lighting",
        tokio::task::spawn_blocking(mouse_lighting::check_available).await?
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
                    "local model; CUDA build: {}; Flash Attention: {}",
                    cfg!(feature = "cuda"),
                    dictation.flash_attention && dictation.use_gpu && cfg!(feature = "cuda")
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

async fn run(connection: Connection, tray_only: bool, config: AppConfig) -> Result<()> {
    let dictation_config = config.dictation;
    let notifications = if config.notifications.enabled && !tray_only {
        Some(
            sie::notifications::Runtime::start(
                &config.notifications,
                &dictation_config.database_url,
                dictation_config.duck_sink.clone(),
                dictation_config.duck_volume_percent,
            )
            .await?,
        )
    } else {
        None
    };
    let notification_control = notifications
        .as_ref()
        .map(|runtime| runtime.control.clone());
    let mut notification_status_rx = notification_control
        .as_ref()
        .map(|c| c.subscribe())
        .unwrap_or_else(|| tokio::sync::watch::channel(sie::notifications::Status::default()).1);
    let mut lighting_mode = LightingMode::On;
    let (lighting_requests, lighting_request_rx) =
        tokio::sync::watch::channel(LightingState::default());
    let (lighting_status, mut lighting_status_rx) = tokio::sync::watch::channel(String::new());
    let (mouse_requests, mouse_request_rx) = tokio::sync::watch::channel(lighting_mode);
    let (mouse_status, mut mouse_status_rx) = tokio::sync::watch::channel(String::new());
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
        mouse_lighting_status: if tray_only {
            "Řízení osvětlení myši vypnuté"
        } else {
            "Připojuji myš…"
        }
        .into(),
        media_status: MediaStatus::default().label(),
        focus_status: None,
        switcher_status: (!tray_only).then(|| "Přepínání aplikací: kontroluji…".into()),
        notification_status: notification_control.as_ref().map(|c| c.status()),
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
    let (dial_requests, dial_rx) = mpsc::channel(16);
    let (dial_feedback, mut dial_feedback_rx) =
        tokio::sync::watch::channel(sie::lighting::DialFeedback::Idle);
    let dial_task = if tray_only {
        None
    } else {
        Some(tokio::spawn(sie::switcher::run(
            connection.clone(),
            dial_rx,
            dial_feedback,
        )))
    };
    let (workspace_changes, mut workspace_rx) = mpsc::channel(1);
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dictation_task = None;
    let lighting_task = if tray_only {
        None
    } else {
        Some(tokio::spawn(openrazer::run(
            connection.clone(),
            lighting_request_rx,
            lighting_status,
        )))
    };
    let mouse_task = if tray_only {
        None
    } else {
        Some(tokio::spawn(mouse_lighting::run(
            mouse_request_rx,
            mouse_status,
        )))
    };
    if !tray_only {
        tasks.spawn(input::monitor(macros.clone()));
        tasks.spawn(sie::dial_input::monitor(macros));
        tasks.spawn(media_focus::run(
            connection.clone(),
            broker,
            focus_rx,
            native_hint_rx,
            media_rx.clone(),
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
            notification_control.clone(),
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
                Some(Action::ToggleNotifications) => if let Some(control)=&notification_control { control.toggle_mute(); },
                Some(Action::StopSpeech) => if let Some(control)=&notification_control { control.stop_current(); },
                Some(Action::SetLightingMode(mode)) => if !tray_only {
                    lighting_mode = mode;
                    mouse_requests.send_if_modified(|old| {
                        if *old == mode { false } else { *old = mode; true }
                    });
                    let media=media_rx.borrow_and_update().clone();
                    let recording = dictation_status_rx.borrow().phase == Phase::Recording;
                    refresh_lighting(&connection, &tray, lighting_mode, &media, recording, &lighting_requests, &mut last_status).await;
                },
                Some(Action::Dictation(command)) => if !tray_only { let _ = dictation_commands.try_send(command); },
                Some(Action::About) => if about.is_empty() {
                    about.spawn(async { if let Err(error) = sie::tray::show_about().await { tracing::warn!(%error, "About dialog failed"); } });
                },
            },
            Some(_) = about.join_next(), if !about.is_empty() => {},
            Ok(()) = notification_status_rx.changed(), if notifications.is_some() => {
                let status=notification_status_rx.borrow_and_update().clone();
                tray.update(move |tray|tray.notification_status=Some(status)).await;
            },
            Some((action, at)) = macro_rx.recv(), if !tray_only => {
                let index = match action {
                    InputAction::DialTurn(delta)=>{let _=dial_requests.try_send(sie::switcher::Command::Turn(delta));continue;},
                    InputAction::DialClick=>{let _=dial_requests.try_send(sie::switcher::Command::Click);continue;},
                    InputAction::DialDisconnected=>{let _=dial_requests.try_send(sie::switcher::Command::Cancel);continue;},
                    InputAction::Workspace(index) => index,
                    InputAction::MediaReveal => {
                        if let Some(fallback) = media_rx.borrow().target.clone() {
                            let _ = focus_requests.try_send(media_focus::Request {
                                at,
                                fallback,
                            });
                        }
                        continue;
                    },
                    InputAction::DictationToggle => { let _ = dictation_commands.try_send(DictationCommand::Toggle); continue; },
                    InputAction::DictationCancel => { let _ = dictation_commands.try_send(DictationCommand::Cancel); continue; },
                    InputAction::DictationSubmit => { let _ = dictation_commands.try_send(DictationCommand::FinishAndSubmit); continue; },
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
            Ok(()) = dial_feedback_rx.changed(), if !tray_only => {
                let dial=*dial_feedback_rx.borrow_and_update();
                lighting_requests.send_if_modified(|state| {
                    if state.dial==dial {false} else {state.dial=dial;true}
                });
            },
            Ok(()) = dictation_status_rx.changed(), if !tray_only => {
                let status=dictation_status_rx.borrow_and_update().clone();
                tracing::info!(phase=?status.phase,"Dictation state changed");
                lighting_requests.send_if_modified(|state| {
                    let recording = status.phase == Phase::Recording;
                    if state.recording == recording { return false; }
                    state.recording = recording;
                    true
                });
                tray.update(move |tray| tray.dictation_status=status).await;
            },
            Ok(()) = lighting_status_rx.changed(), if !tray_only => {
                let status = lighting_status_rx.borrow_and_update().clone();
                tray.update(move |tray| tray.lighting_status = status).await;
            },
            Ok(()) = mouse_status_rx.changed(), if !tray_only => {
                let status = mouse_status_rx.borrow_and_update().clone();
                tray.update(move |tray| tray.mouse_lighting_status = status).await;
            },
            _ = async {
                tokio::select! {
                    _ = refresh.tick() => {},
                    Some(()) = workspace_rx.recv() => {},
                    Ok(()) = media_rx.changed() => {},
                }
            }, if !tray_only => {
                let ready = sie::switcher::available(&connection).await;
                tray.update(move |tray| tray.switcher_status = Some(if ready {
                    "Přepínání aplikací: připraveno"
                } else {
                    "Přepínání aplikací: odhlaste se a znovu přihlaste"
                }.into())).await;
                let media=media_rx.borrow_and_update().clone();
                let recording = dictation_status_rx.borrow().phase == Phase::Recording;
                refresh_lighting(&connection, &tray, lighting_mode, &media, recording, &lighting_requests, &mut last_status).await;
            },
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::error!(%error, "Input monitor stopped"); }
            },
        }
    }
    if let Some(runtime) = &notifications {
        runtime.request_shutdown();
    }
    drop(dictation_commands);
    drop(dial_requests);
    if let Some(mut task) = dial_task
        && tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    drop(lighting_requests);
    drop(mouse_requests);
    if let Some(mut task) = mouse_task
        && tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    if let Some(mut task) = lighting_task
        && tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    if let Some(mut task) = dictation_task
        && tokio::time::timeout(Duration::from_secs(4), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    if let Some(notifications) = notifications {
        notifications.shutdown().await;
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
    recording: bool,
    lighting_requests: &tokio::sync::watch::Sender<LightingState>,
    last_status: &mut String,
) {
    let state = desktop::workspace_state(connection).await.ok();
    let active = state.and_then(|state| state.active_macro());
    let next = LightingState {
        mode,
        workspace: active,
        playback: media.playback,
        recording,
        dial: lighting_requests.borrow().dial,
    };
    lighting_requests.send_if_modified(|old| {
        if *old == next {
            return false;
        }
        *old = next;
        true
    });
    let workspace = state
        .map(sie::tray::workspace_label)
        .unwrap_or_else(|| "Ovládání ploch nedostupné".into());
    let media = media.label();
    let status = format!("{mode:?}; {workspace}; {media}");
    if status != *last_status {
        tracing::info!("{status}");
        *last_status = status;
        tray.update(move |tray| {
            tray.lighting_mode = mode;
            tray.workspace_status = workspace;
            tray.media_status = media;
        })
        .await;
    }
}
