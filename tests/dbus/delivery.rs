use super::*;

#[tokio::test]
async fn canceled_dictation_cannot_touch_the_clipboard_or_start_an_input_session() {
    use std::sync::atomic::AtomicBool;
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let target: sie::desktop::FocusTarget =
        serde_json::from_value(serde_json::json!({"id":1,"wm_class":"Alacritty","focus":true}))
            .unwrap();
    let result = sie::desktop::paste_text(
        &bus.connection().await,
        &target,
        "Do not paste",
        Arc::new(AtomicBool::new(true)),
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("canceled"));
    let state = state.lock().unwrap();
    assert!(state.keys.is_empty());
    assert_eq!(state.starts, 0);
}

#[tokio::test]
async fn failed_canceled_refocused_and_paste_only_deliveries_never_send_enter() {
    use sie::desktop::{DeliveryMode, FocusTarget};
    for scenario in [
        "paste-only",
        "paste-failure",
        "cancel-before",
        "cancel-after",
        "refocus-before",
        "refocus-after",
    ] {
        let bus = Bus::new();
        let cancelled = Arc::new(AtomicBool::new(scenario == "cancel-before"));
        let state = Arc::new(Mutex::new(State {
            focused_window: if scenario == "refocus-before" { 2 } else { 1 },
            fail_key: (scenario == "paste-failure").then_some(47),
            switch_on_paste: scenario == "refocus-after",
            cancel_on_paste: (scenario == "cancel-after").then_some(cancelled.clone()),
            ..Default::default()
        }));
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let connection = bus.connection().await;
        let target: FocusTarget =
            serde_json::from_value(serde_json::json!({"id":1,"wm_class":"org.gnome.TextEditor"}))
                .unwrap();
        let desktop = Desktop::connect(&connection).await.unwrap();
        let mode = if scenario == "paste-only" {
            DeliveryMode::PasteOnly
        } else {
            DeliveryMode::Submit
        };
        let result = desktop
            .paste_shortcut(&[29, 47], &target, mode, &cancelled)
            .await;
        assert_eq!(result.is_ok(), scenario == "paste-only", "{scenario}");
        desktop.stop().await.unwrap();
        let state = state.lock().unwrap();
        assert!(!state.keys.iter().any(|(key, _)| *key == 28), "{scenario}");
        if ["cancel-before", "refocus-before"].contains(&scenario) {
            assert!(state.keys.is_empty());
        } else {
            assert_eq!(
                state.keys,
                [(29, true), (47, true), (47, false), (29, false)]
            );
        }
    }
}

#[tokio::test]
async fn empty_or_canceled_submit_cannot_stage_text_or_start_input() {
    use sie::desktop::{DeliveryMode, FocusTarget};
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    let target: FocusTarget =
        serde_json::from_value(serde_json::json!({"id":1,"wm_class":"org.gnome.TextEditor"}))
            .unwrap();
    for text in ["", " \n\t"] {
        assert!(
            sie::desktop::paste_text_with_receipt(
                &connection,
                &target,
                text,
                Arc::new(AtomicBool::new(false))
            )
            .await
            .unwrap()
            .is_none()
        );
        sie::desktop::paste_text_with_mode(
            &connection,
            &target,
            text,
            DeliveryMode::Submit,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    }
    assert!(
        sie::desktop::paste_text_with_mode(
            &connection,
            &target,
            "Canceled",
            DeliveryMode::Submit,
            Arc::new(AtomicBool::new(true))
        )
        .await
        .is_err()
    );
    let state = state.lock().unwrap();
    assert!(state.keys.is_empty());
    assert_eq!(state.starts, 0);
}

#[tokio::test]
async fn paste_shortcut_failure_still_releases_the_character_and_modifier() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        fail_key: Some(47),
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let desktop = Desktop::connect(&bus.connection().await).await.unwrap();
    assert!(desktop.send_keys(&[29, 47]).await.is_err());
    desktop.stop().await.unwrap();
    assert_eq!(
        state.lock().unwrap().keys,
        [(29, true), (47, true), (47, false), (29, false)]
    );
}

#[tokio::test]
async fn submit_sends_one_enter_after_each_successful_clipboard_paste_path() {
    use sie::desktop::{DeliveryMode, FocusTarget};
    for keys in [&[29, 47][..], &[29, 42, 47][..], &[42, 110][..]] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            focused_window: 1,
            ..Default::default()
        }));
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let connection = bus.connection().await;
        let target: FocusTarget =
            serde_json::from_value(serde_json::json!({"id":1,"wm_class":"org.gnome.TextEditor"}))
                .unwrap();
        let desktop = Desktop::connect(&connection).await.unwrap();
        desktop
            .paste_shortcut(keys, &target, DeliveryMode::Submit, &AtomicBool::new(false))
            .await
            .unwrap();
        desktop.stop().await.unwrap();
        let mut expected: Vec<_> = keys.iter().map(|key| (*key, true)).collect();
        expected.extend(keys.iter().rev().map(|key| (*key, false)));
        expected.extend([(28, true), (28, false)]);
        let state = state.lock().unwrap();
        assert_eq!(state.keys, expected);
        assert_eq!((state.starts, state.stops), (1, 1));
    }
}

#[tokio::test]
async fn standalone_enter_sends_only_enter_and_cleans_up_failures_or_changed_focus() {
    for scenario in ["normal", "press-failure", "refocus", "cancel-before"] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            focused_window: 1,
            fail_key: (scenario == "press-failure").then_some(28),
            switch_on_start: scenario == "refocus",
            ..Default::default()
        }));
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let connection = bus.connection().await;
        let result = if scenario == "cancel-before" {
            let target = sie::desktop::focused_window(&connection).await.unwrap();
            sie::desktop::send_enter_to(&connection, &target, &AtomicBool::new(true)).await
        } else {
            sie::desktop::press_enter(&connection).await
        };
        assert_eq!(result.is_ok(), scenario == "normal", "{scenario}");
        let state = state.lock().unwrap();
        if ["normal", "press-failure"].contains(&scenario) {
            assert_eq!(state.keys, [(28, true), (28, false)]);
        } else {
            assert!(state.keys.is_empty());
        }
        let sessions = usize::from(scenario != "cancel-before");
        assert_eq!((state.starts, state.stops), (sessions, sessions));
    }
}

#[tokio::test]
async fn successful_paste_receipt_submits_later_but_cancel_or_refocus_suppresses_enter() {
    use sie::desktop::{DeliveryMode, FocusTarget};
    for scenario in ["submit-later", "cancel-later", "refocus-later"] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            focused_window: 1,
            ..Default::default()
        }));
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let connection = bus.connection().await;
        let target: FocusTarget =
            serde_json::from_value(serde_json::json!({"id":1,"wm_class":"org.gnome.TextEditor"}))
                .unwrap();
        let cancelled = AtomicBool::new(false);
        let desktop = Desktop::connect(&connection).await.unwrap();
        let receipt = desktop
            .paste_shortcut(&[29, 47], &target, DeliveryMode::PasteOnly, &cancelled)
            .await
            .unwrap();
        desktop.stop().await.unwrap();
        assert!(!state.lock().unwrap().keys.iter().any(|(key, _)| *key == 28));
        if scenario == "cancel-later" {
            cancelled.store(true, Ordering::SeqCst);
        }
        if scenario == "refocus-later" {
            state.lock().unwrap().focused_window = 2;
        }
        let result = receipt.submit(&connection, &cancelled).await;
        assert_eq!(result.is_ok(), scenario == "submit-later");
        let state = state.lock().unwrap();
        assert_eq!(
            state
                .keys
                .iter()
                .filter(|(key, pressed)| *key == 28 && *pressed)
                .count(),
            usize::from(scenario == "submit-later")
        );
        assert_eq!(state.starts, state.stops);
    }
}

#[tokio::test]
async fn missing_or_outdated_native_extension_never_falls_back_to_immediate_local_activation() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    for version in [None, Some(1), Some(2), Some(4)] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            focused_window: 1,
            native_version: version,
            switcher_windows: Some(vec![
                serde_json::json!({"id":1,"wm_class":"First","workspace":0}),
                serde_json::json!({"id":2,"wm_class":"Second","workspace":0}),
            ]),
            ..Default::default()
        }));
        let server = services(&bus, state.clone(), vec![8, 23]).await;
        if version.is_none() {
            server.release_name(switcher::NATIVE_BUS).await.unwrap();
        }
        let connection = bus.connection().await;
        assert!(!switcher::available(&connection).await);
        let (commands, receiver) = mpsc::channel(4);
        let (feedback, feedback_rx) = watch::channel(DialFeedback::Idle);
        let task = tokio::spawn(switcher::run(connection, receiver, feedback));
        commands.send(Command::Click).await.unwrap();
        commands.send(Command::Cancel).await.unwrap();
        commands.send(Command::Turn(1)).await.unwrap();
        drop(commands);
        task.await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.native_opens, usize::from(version == Some(2)));
        assert!(state.native_local_opens.is_empty());
        assert!(state.activated_windows.is_empty());
        assert!(state.keys.is_empty());
        assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
    }
}
