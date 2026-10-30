//! Native notification protocol on a private bus, with no desktop or input.
#[path = "support/bus.rs"]
mod private_bus;
use private_bus::Bus;
use sie::{agent_notifications, agent_origin::Question};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use zbus::connection::Builder;
use zbus::{Connection, zvariant::OwnedValue};

const PATH: &str = "/org/freedesktop/Notifications";
const IFACE: &str = "org.freedesktop.Notifications";
#[derive(Default)]
struct State {
    calls: usize,
    summary: String,
    body: String,
    actions: Vec<String>,
    sound_suppressed: bool,
    urgency: u8,
    closed: Vec<u32>,
}
struct Notifications(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".into(), "body".into()]
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: &str,
        _replaces: u32,
        _icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        _timeout: i32,
    ) -> u32 {
        let mut s = self.0.lock().unwrap();
        s.calls += 1;
        s.summary = summary.into();
        s.body = body.into();
        s.actions = actions;
        s.sound_suppressed = hints
            .get("suppress-sound")
            .and_then(|v| bool::try_from(v).ok())
            == Some(true);
        s.urgency = hints
            .get("urgency")
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(0);
        42
    }
    fn close_notification(&self, id: u32) {
        self.0.lock().unwrap().closed.push(id);
    }
}
async fn server(bus: &Bus, state: Arc<Mutex<State>>) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(PATH, Notifications(state))
        .unwrap()
        .name(IFACE)
        .unwrap()
        .build()
        .await
        .unwrap()
}
async fn notified(state: &Arc<Mutex<State>>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.lock().unwrap().calls == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn question() -> Question {
    Question {
        agent: "codex".into(),
        session_id: "session-1".into(),
        cwd: "/project".into(),
        text: "Je <okno> správně & viditelné?".into(),
    }
}

#[tokio::test]
async fn only_own_default_or_button_action_activates_once_and_keeps_audio_independent() {
    for action in ["default", "open"] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let server = server(&bus, state.clone()).await;
        let client = bus.connection().await;
        let called = Arc::new(AtomicUsize::new(0));
        let activated = called.clone();
        let task = tokio::spawn(async move {
            agent_notifications::present(
                &client,
                &question(),
                3,
                || async {
                    activated.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                std::future::pending(),
            )
            .await
        });
        notified(&state).await;
        server
            .emit_signal(
                None::<&str>,
                PATH,
                IFACE,
                "ActionInvoked",
                &(99u32, "default"),
            )
            .await
            .unwrap();
        server
            .emit_signal(
                None::<&str>,
                PATH,
                IFACE,
                "ActionInvoked",
                &(42u32, "unexpected"),
            )
            .await
            .unwrap();
        server
            .emit_signal(None::<&str>, PATH, IFACE, "ActionInvoked", &(42u32, action))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 1);
        let s = state.lock().unwrap();
        assert!(s.summary.contains("Plocha 4"));
        assert!(s.body.contains("&lt;okno&gt;"));
        assert!(s.sound_suppressed);
        assert_eq!(
            s.urgency, 2,
            "Questions must appear during fullscreen playback"
        );
        assert_eq!(s.closed, [42]);
        assert_eq!(
            s.actions,
            ["default", "Otevřít otázku", "open", "Přejít k otázce"]
        );
    }
}

#[tokio::test]
async fn resolved_dismissed_or_previously_canceled_questions_never_activate() {
    for scenario in ["resolved", "dismissed", "before-show"] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let server = server(&bus, state.clone()).await;
        let client = bus.connection().await;
        let (cancel, receive) = tokio::sync::oneshot::channel();
        if scenario == "before-show" {
            cancel.send(()).unwrap();
            agent_notifications::present(
                &client,
                &question(),
                3,
                || async { panic!("Canceled question activated") },
                async {
                    let _ = receive.await;
                },
            )
            .await
            .unwrap();
            assert_eq!(state.lock().unwrap().calls, 0);
            continue;
        }
        let task = tokio::spawn(async move {
            agent_notifications::present(
                &client,
                &question(),
                3,
                || async { panic!("Canceled question activated") },
                async {
                    let _ = receive.await;
                },
            )
            .await
        });
        notified(&state).await;
        if scenario == "resolved" {
            cancel.send(()).unwrap();
        } else {
            server
                .emit_signal(
                    None::<&str>,
                    PATH,
                    IFACE,
                    "NotificationClosed",
                    &(42u32, 2u32),
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(state.lock().unwrap().closed, [42]);
    }
}

#[tokio::test]
async fn closed_origin_activation_error_still_removes_its_notification() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let server = server(&bus, state.clone()).await;
    let client = bus.connection().await;
    let task = tokio::spawn(async move {
        agent_notifications::present(
            &client,
            &question(),
            3,
            || async { anyhow::bail!("Target closed") },
            std::future::pending(),
        )
        .await
    });
    notified(&state).await;
    server
        .emit_signal(
            None::<&str>,
            PATH,
            IFACE,
            "ActionInvoked",
            &(42u32, "default"),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(state.lock().unwrap().closed, [42]);
}
