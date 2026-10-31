//! Desktop application composition and event coordination.
mod projection;
mod routing;
mod workers;
use anyhow::{Context, Result};
use ksni::TrayMethods;
use projection::Lighting;
use routing::{Router, switch_workspace};
use sie::{
    browser,
    config::AppConfig,
    desktop,
    dictation::{self, Phase, Status as DictationStatus},
    input,
    lighting::{LightingMode, State as LightingState},
    media::{self, Status as MediaStatus},
    media_focus, mouse_lighting, openrazer,
    tray::{Action, AppTray},
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};
use workers::Worker;
use zbus::Connection;

pub async fn run(connection: Connection, tray_only: bool, config: AppConfig) -> Result<()> {
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
    let lighting_mode = LightingMode::On;
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
        Some(Worker::spawn(sie::switcher::run(
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
        Some(Worker::spawn(openrazer::run(
            connection.clone(),
            lighting_request_rx,
            lighting_status,
        )))
    };
    let mouse_task = if tray_only {
        None
    } else {
        Some(Worker::spawn(mouse_lighting::run(
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
        dictation_task = Some(Worker::spawn(dictation::run(
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
    let mut lighting = Lighting::new(lighting_requests, mouse_requests);
    let router = Router {
        dictation: dictation_commands.clone(),
        dial: dial_requests.clone(),
        focus: focus_requests,
    };
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
                    lighting.set_mode(mode);
                    let media=media_rx.borrow_and_update().clone();
                    let recording = dictation_status_rx.borrow().phase == Phase::Recording;
                    lighting.refresh(&connection, &tray, &media, recording).await;
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
                let media = media_rx.borrow().clone();
                if let Some(index) = router.route(action, at, &media) {
                    switch_workspace(&connection, index).await;
                }
            },
            Ok(()) = focus_status_rx.changed(), if !tray_only => {
                let status = focus_status_rx.borrow_and_update().clone();
                tray.update(move |tray| tray.focus_status = status).await;
            },
            Ok(()) = dial_feedback_rx.changed(), if !tray_only => {
                let dial=*dial_feedback_rx.borrow_and_update();
                lighting.set_dial(dial);
            },
            Ok(()) = dictation_status_rx.changed(), if !tray_only => {
                let status=dictation_status_rx.borrow_and_update().clone();
                tracing::info!(phase=?status.phase,"Dictation state changed");
                lighting.set_recording(status.phase == Phase::Recording);
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
                lighting.refresh(&connection, &tray, &media, recording).await;
            },
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::error!(%error, "Input monitor stopped"); }
            },
        }
    }
    if let Some(runtime) = &notifications {
        runtime.request_shutdown();
    }
    drop(router);
    drop(dictation_commands);
    drop(dial_requests);
    workers::shutdown(dial_task, Duration::from_secs(3)).await;
    drop(lighting);
    workers::shutdown(mouse_task, Duration::from_secs(3)).await;
    workers::shutdown(lighting_task, Duration::from_secs(3)).await;
    workers::shutdown(dictation_task, Duration::from_secs(4)).await;
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
