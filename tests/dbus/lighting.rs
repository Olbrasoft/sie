use super::*;

#[tokio::test]
async fn lighting_writes_a_byte_array_only_to_the_supported_model() {
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    assert_eq!(
        openrazer::apply(&bus.connection().await, None)
            .await
            .unwrap(),
        1
    );
    let state = state.lock().unwrap();
    assert_eq!(state.frames, vec![encode_frame(&touch_typing_frame())]);
    assert_eq!(state.customs, 1);
}

#[tokio::test]
async fn wrong_matrix_prevents_all_lighting_writes() {
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![6, 22]).await;
    assert!(
        openrazer::apply(&bus.connection().await, None)
            .await
            .is_err()
    );
    assert!(state.lock().unwrap().frames.is_empty());
}

#[tokio::test]
async fn externally_changed_workspace_moves_the_led_without_sending_keyboard_input() {
    use sie::desktop::{watch_workspaces, workspace_state};
    use std::time::Duration;

    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        ..Default::default()
    }));
    let server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    let (changed, mut changes) = tokio::sync::mpsc::channel(1);
    let watcher = tokio::spawn(watch_workspaces(connection.clone(), changed));
    tokio::time::timeout(Duration::from_secs(2), changes.recv())
        .await
        .unwrap()
        .unwrap();

    // These changes originate at the compositor, without any macro key event.
    for index in [2, 3] {
        state.lock().unwrap().current = index;
        server
            .emit_signal(
                None::<&str>,
                "/org/olbrasoft/Desktop",
                "org.olbrasoft.Desktop",
                "WorkspaceChanged",
                &(index, 5_i32),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), changes.recv())
            .await
            .unwrap()
            .unwrap();
        let desktop = workspace_state(&connection).await.unwrap();
        assert_eq!(desktop.index, index);
        openrazer::apply(&connection, desktop.active_macro())
            .await
            .unwrap();
    }
    let state = state.lock().unwrap();
    let third = &state.frames[0];
    let fourth = &state.frames[1];
    assert_eq!(
        &third[3 * 72 + 6..3 * 72 + 9],
        &[0, 255, 0],
        "M3 on workspace 3"
    );
    assert_eq!(
        &third[2 * 72 + 6..2 * 72 + 9],
        &[255, 255, 255],
        "M4 initially white"
    );
    assert_eq!(
        &fourth[3 * 72 + 6..3 * 72 + 9],
        &[255, 255, 255],
        "M3 returns to white"
    );
    assert_eq!(
        &fourth[2 * 72 + 6..2 * 72 + 9],
        &[0, 255, 0],
        "M4 on workspace 4"
    );
    assert!(state.keys.is_empty());
    assert_eq!(state.starts, 0, "Lighting must not open an input session");
    watcher.abort();
}

#[tokio::test]
async fn losing_workspace_state_clears_the_old_highlight() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        current: 2,
        ..Default::default()
    }));
    let server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    let active = sie::desktop::workspace_state(&connection)
        .await
        .unwrap()
        .active_macro();
    openrazer::apply(&connection, active).await.unwrap();
    server.release_name("org.olbrasoft.Desktop").await.unwrap();
    let active = sie::desktop::workspace_state(&connection)
        .await
        .ok()
        .and_then(|state| state.active_macro());
    assert!(active.is_none());
    openrazer::apply(&connection, active).await.unwrap();
    assert_eq!(
        state.lock().unwrap().frames.last().unwrap(),
        &encode_frame(&touch_typing_frame())
    );
}

#[tokio::test]
async fn lighting_toggle_stays_dark_during_workspace_control_and_restores_the_current_profile() {
    use sie::lighting::{LightingMode, workspace_frame};

    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        current: 2,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    let mut mode = LightingMode::On;
    openrazer::apply_mode(&connection, Some(2), mode)
        .await
        .unwrap();

    mode = mode.toggled();
    openrazer::apply_mode(&connection, Some(2), mode)
        .await
        .unwrap();
    let output = switch(&bus, &schemas(), 3).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let active = sie::desktop::workspace_state(&connection)
        .await
        .unwrap()
        .active_macro();
    assert_eq!(active, Some(3));
    // The workspace event and a subsequent periodic refresh must both stay dark.
    openrazer::apply_mode(&connection, active, mode)
        .await
        .unwrap();
    openrazer::apply_mode(&connection, active, mode)
        .await
        .unwrap();

    mode = mode.toggled();
    openrazer::apply_mode(&connection, active, mode)
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.frames.len(), 5);
    for frame in &state.frames[1..=3] {
        assert_eq!(frame.len(), 576);
        for row in frame.as_chunks::<72>().0 {
            assert!(
                row[3..].iter().all(|byte| *byte == 0),
                "Every RGB cell must stay black"
            );
        }
    }
    assert_eq!(state.frames[0], encode_frame(&workspace_frame(Some(2))));
    assert_eq!(state.frames[4], encode_frame(&workspace_frame(Some(3))));
    assert_eq!(
        state.keys,
        [(125, true), (75, true), (75, false), (125, false)]
    );
    assert_eq!((state.starts, state.stops), (1, 1));
}

#[tokio::test]
async fn recording_renderer_preserves_keys_handles_off_reconnect_and_restores_idle_on_shutdown() {
    use sie::{
        lighting::{LightingMode, State as LightingState},
        media::Playback,
    };
    use tokio::sync::watch;
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let mut settings = LightingState {
        workspace: Some(2),
        playback: Playback::Paused,
        ..Default::default()
    };
    let (requests, receiver) = watch::channel(settings);
    let (status, mut status_rx) = watch::channel(String::new());
    let task = tokio::spawn(sie::openrazer::run(
        bus.connection().await,
        receiver,
        status,
    ));
    wait_for_frames(&state, 1).await;
    let idle = encode_frame(&LightingMode::On.frame_with_media(Some(2), Playback::Paused));
    assert_eq!(state.lock().unwrap().frames[0], idle);
    settings.recording = true;
    requests.send_replace(settings);
    wait_for_frames(&state, 4).await;
    {
        let state = state.lock().unwrap();
        assert!(state.frames[1..].windows(2).any(|w| w[0] != w[1]));
        for frame in &state.frames[1..] {
            // All six key rows remain identical to the idle frame.
            assert_eq!(&frame[..432], &idle[..432]);
            assert_ne!(&frame[432..], &idle[432..]);
        }
    }
    settings.workspace = Some(3);
    settings.playback = Playback::Playing;
    let before = state.lock().unwrap().frames.len();
    requests.send_replace(settings);
    let expected = encode_frame(&LightingMode::On.frame_with_media(Some(3), Playback::Playing));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if state.lock().unwrap().frames[before..]
                .iter()
                .any(|f| f[..432] == expected[..432])
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    settings.mode = LightingMode::Off;
    requests.send_replace(settings);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if state
                .lock()
                .unwrap()
                .frames
                .last()
                .is_some_and(|f| f[3..72].iter().all(|b| *b == 0))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let dark_from = state.lock().unwrap().frames.len() - 1;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for frame in &state.lock().unwrap().frames[dark_from..] {
        for row in frame.as_chunks::<72>().0 {
            assert!(row[3..].iter().all(|b| *b == 0));
        }
    }
    // Continue recording while off, reconnect, then enable: use the full current
    // profile immediately, rather than initializing only the ambient rows.
    state.lock().unwrap().missing_keyboard = true;
    settings.mode = LightingMode::On;
    requests.send_replace(settings);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let missing = status_rx.borrow_and_update().contains("není připojena");
            if missing {
                break;
            }
            status_rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let before = state.lock().unwrap().frames.len();
    state.lock().unwrap().missing_keyboard = false;
    wait_for_frames(&state, before + 1).await;
    let restored = state.lock().unwrap().frames.last().unwrap().clone();
    assert_eq!(&restored[..432], &expected[..432]);
    assert_ne!(&restored[432..], &expected[432..]);
    settings.recording = false;
    requests.send_replace(settings);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if state.lock().unwrap().frames.last() == Some(&expected) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Quit during another recording must leave the ambient strip white.
    settings.recording = true;
    let before = state.lock().unwrap().frames.len();
    requests.send_replace(settings);
    wait_for_frames(&state, before + 1).await;
    drop(requests);
    tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.lock().unwrap().frames.last(), Some(&expected));
}
