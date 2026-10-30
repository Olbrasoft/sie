use super::*;

#[tokio::test]
async fn native_dial_click_turn_confirm_never_injects_keyboard_modifiers_or_opens_a_view() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let (commands, receiver) = mpsc::channel(8);
    let (feedback, mut feedback_rx) = watch::channel(DialFeedback::Idle);
    let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
    commands.send(Command::Click).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        feedback_rx.wait_for(|v| *v == DialFeedback::Global),
    )
    .await
    .unwrap()
    .unwrap();
    commands.send(Command::Turn(2)).await.unwrap();
    commands.send(Command::Turn(-1)).await.unwrap();
    commands.send(Command::Click).await.unwrap();
    wait_for_native(&state, |s| !s.native_finishes.is_empty()).await;
    drop(commands);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.native_steps, [2, -1]);
    assert_eq!(state.native_finishes, [true]);
    assert_eq!(state.native_opens, 1);
    assert!(
        state.activated_windows.is_empty(),
        "GNOME owns native activation"
    );
    assert!(state.keys.is_empty());
    assert_eq!(state.starts, 0);
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
}

#[tokio::test]
async fn native_dismissal_disconnect_and_shutdown_restore_idle_without_confirmation() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let (commands, receiver) = mpsc::channel(8);
    let (feedback, mut feedback_rx) = watch::channel(DialFeedback::Idle);
    let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
    for pass in 0..3 {
        commands.send(Command::Click).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            feedback_rx.wait_for(|v| *v == DialFeedback::Global),
        )
        .await
        .unwrap()
        .unwrap();
        if pass == 0 {
            state.lock().unwrap().native_active = false; // Native Escape/mouse/idle dismissal.
        } else if pass == 1 {
            commands.send(Command::Cancel).await.unwrap(); // Dial disconnect.
        } else {
            break;
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            feedback_rx.wait_for(|v| *v == DialFeedback::Idle),
        )
        .await
        .unwrap()
        .unwrap();
    }
    drop(commands); // Quit with an open native popup.
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.native_finishes, [false, false]);
    assert!(!state.native_active);
    assert!(state.keys.is_empty());
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
}

#[tokio::test]
async fn native_move_failure_cancels_the_owned_popup_and_restores_white() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        native_fail_move: true,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let (commands, receiver) = mpsc::channel(4);
    let (feedback, mut feedback_rx) = watch::channel(DialFeedback::Idle);
    let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
    commands.send(Command::Click).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        feedback_rx.wait_for(|v| *v == DialFeedback::Global),
    )
    .await
    .unwrap()
    .unwrap();
    commands.send(Command::Turn(1)).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        feedback_rx.wait_for(|v| *v == DialFeedback::Idle),
    )
    .await
    .unwrap()
    .unwrap();
    drop(commands);
    task.await.unwrap();
    assert_eq!(state.lock().unwrap().native_finishes, [false]);
    assert!(!state.lock().unwrap().native_active);
}

#[tokio::test]
async fn local_dial_activation_validates_workspace_and_window_before_activation() {
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    for (id, workspace) in [(2, Some(0)), (1, Some(1)), (99, None)] {
        assert!(
            sie::switcher::activate(&connection, id, workspace)
                .await
                .is_err()
        );
    }
    assert!(state.lock().unwrap().activated_windows.is_empty());
    sie::switcher::activate(&connection, 1, Some(0))
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().activated_windows, [1]);
    sie::switcher::activate(&connection, 2, None).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.current, 1);
    assert_eq!(state.focused_window, 2);
    assert_eq!(state.activated_windows, [1, 2]);
    assert!(
        state.keys.is_empty(),
        "Window activation never holds synthetic Alt"
    );
    assert_eq!(state.starts, 0);
}

#[tokio::test]
async fn local_dial_worker_previews_without_activation_and_confirms_on_click() {
    use sie::{lighting::DialFeedback, switcher};
    use tokio::sync::{mpsc, watch};
    let bus = Bus::new();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let (commands, receiver) = mpsc::channel(4);
    let (feedback, mut feedback_rx) = watch::channel(DialFeedback::Idle);
    let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
    commands.send(switcher::Command::Turn(1)).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        feedback_rx.wait_for(|v| *v == DialFeedback::Local),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Local);
    assert!(state.lock().unwrap().activated_windows.is_empty());
    assert!(state.lock().unwrap().native_finishes.is_empty());
    commands.send(switcher::Command::Turn(-1)).await.unwrap();
    wait_for_native(&state, |s| !s.native_steps.is_empty()).await;
    commands.send(switcher::Command::Click).await.unwrap();
    wait_for_native(&state, |s| !s.native_finishes.is_empty()).await;
    drop(commands);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.native_local_opens, [1]);
    assert_eq!(state.native_steps, [-1]);
    assert_eq!(state.native_finishes, [true]);
    assert_eq!(state.native_opens, 0);
    assert!(state.activated_windows.is_empty());
    assert!(state.keys.is_empty());
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
    assert_eq!(state.current, 0);
}

#[tokio::test]
async fn local_first_delta_is_forwarded_once_and_zero_does_not_open_a_picker() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    for delta in [0, -1, -3, 2] {
        let bus = Bus::new();
        let state = Shared::default();
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let (commands, receiver) = mpsc::channel(4);
        let (feedback, feedback_rx) = watch::channel(DialFeedback::Idle);
        let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
        commands.send(Command::Turn(delta)).await.unwrap();
        drop(commands);
        task.await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(
            state.native_local_opens,
            if delta == 0 { vec![] } else { vec![delta] }
        );
        assert!(state.native_steps.is_empty());
        assert_eq!(
            state.native_finishes,
            if delta == 0 { vec![] } else { vec![false] }
        );
        assert!(state.activated_windows.is_empty());
        assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
    }
}

#[tokio::test]
async fn local_dismissal_disconnect_and_errors_restore_idle_without_activation() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    for reason in ["native", "cancel", "move-error", "open-error"] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            native_fail_move: reason == "move-error",
            native_fail_open: reason == "open-error",
            ..Default::default()
        }));
        let _server = services(&bus, state.clone(), vec![8, 23]).await;
        let (commands, receiver) = mpsc::channel(4);
        let (feedback, mut feedback_rx) = watch::channel(DialFeedback::Idle);
        let task = tokio::spawn(switcher::run(bus.connection().await, receiver, feedback));
        commands.send(Command::Turn(-1)).await.unwrap();
        if reason != "open-error" {
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                feedback_rx.wait_for(|v| *v == DialFeedback::Local),
            )
            .await
            .unwrap()
            .unwrap();
            match reason {
                "native" => state.lock().unwrap().native_active = false,
                "cancel" => commands.send(Command::Cancel).await.unwrap(),
                _ => commands.send(Command::Turn(1)).await.unwrap(),
            }
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                feedback_rx.wait_for(|v| *v == DialFeedback::Idle),
            )
            .await
            .unwrap()
            .unwrap();
        }
        drop(commands);
        task.await.unwrap();
        let state = state.lock().unwrap();
        assert!(state.native_finishes.iter().all(|confirm| !confirm));
        assert!(!state.native_active);
        assert!(state.activated_windows.is_empty());
        assert!(state.keys.is_empty());
        assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
    }
}
