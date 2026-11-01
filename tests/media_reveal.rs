//! Private bus/native-host checks; no user browser, compositor or keyboard.
#[path = "support/bus.rs"]
mod private_bus;
use private_bus::Bus;
use sie::{browser, media, media_focus};
use std::process::Stdio;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::process::Command as AsyncCommand;
use tokio::sync::{mpsc, watch};
use zbus::connection::Builder;
use zbus::{
    Connection,
    zvariant::{OwnedValue, Str},
};
#[derive(Default)]
struct State {
    activated: Option<u32>,
    workspace: u32,
    raises: usize,
    toggles: usize,
}
struct Root(Arc<Mutex<State>>);
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn identity(&self) -> &str {
        "VLC"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "vlc"
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    fn raise(&self) {
        self.0.lock().unwrap().raises += 1;
    }
}
struct Player(Arc<Mutex<State>>);
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn metadata(&self) -> std::collections::HashMap<String, OwnedValue> {
        std::collections::HashMap::from([(
            "xesam:title".into(),
            OwnedValue::from(Str::from("Video")),
        )])
    }
    fn play_pause(&self) {
        self.0.lock().unwrap().toggles += 1;
    }
}
struct Windows(Arc<Mutex<State>>);
#[zbus::interface(name = "org.gnome.Shell.Extensions.Windows")]
impl Windows {
    fn list(&self) -> String {
        let s = self.0.lock().unwrap();
        serde_json::json!([
        {"id":1,"pid":std::process::id(),"wm_class":"Alacritty","title":"Terminal","workspace":0,"focus":s.activated.is_none()},
        {"id":2,"pid":std::process::id(),"wm_class":"vlc","title":"Video - VLC media player","workspace":4,"focus":s.activated==Some(2)}]).to_string()
    }
    fn activate(&self, id: u32) {
        let mut s = self.0.lock().unwrap();
        assert_eq!(id, 2);
        s.activated = Some(id);
        s.workspace = 4;
    }
}

async fn serve_player(bus: &Bus, state: &Arc<Mutex<State>>) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.mpris.MediaPlayer2.vlc")
        .unwrap()
        .name("org.gnome.Shell")
        .unwrap()
        .serve_at(media::PLAYER_PATH, Root(state.clone()))
        .unwrap()
        .serve_at(media::PLAYER_PATH, Player(state.clone()))
        .unwrap()
        .serve_at(
            "/org/gnome/Shell/Extensions/Windows",
            Windows(state.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn playback(value: media::Playback, before: Option<media::Playback>) -> media::Status {
    media::Status {
        playback: value,
        player: Some("VLC".into()),
        target: Some("org.mpris.MediaPlayer2.vlc".into()),
        transition: before.map(|before| media::Transition {
            at: Instant::now(),
            before,
        }),
    }
}

async fn run_press(
    bus: &Bus,
    initial: media::Status,
    destination: &str,
) -> (
    tokio::task::JoinHandle<()>,
    watch::Sender<media::Status>,
    watch::Receiver<Option<String>>,
    mpsc::Sender<media_focus::Request>,
) {
    run_press_at(bus, initial, destination, Instant::now()).await
}

async fn run_press_at(
    bus: &Bus,
    initial: media::Status,
    destination: &str,
    pressed: Instant,
) -> (
    tokio::task::JoinHandle<()>,
    watch::Sender<media::Status>,
    watch::Receiver<Option<String>>,
    mpsc::Sender<media_focus::Request>,
) {
    let (requests, receiver) = mpsc::channel(8);
    let (hints, hint_rx) = watch::channel(Some(media_focus::NativeHint {
        at: pressed,
        destination: destination.into(),
    }));
    let (states, state_rx) = watch::channel(initial);
    let (status, status_rx) = watch::channel(Some("Awaiting action".into()));
    let task = tokio::spawn(media_focus::run(
        bus.connection().await,
        Arc::new(browser::Broker::default()),
        receiver,
        hint_rx,
        state_rx,
        status,
    ));
    requests
        .send(media_focus::Request {
            at: pressed,
            fallback: "org.mpris.MediaPlayer2.vlc".into(),
        })
        .await
        .unwrap();
    // Keep the hint available; closing the request channel only ends the actor
    // after its current request completes.
    drop(hints);
    (task, states, status_rx, requests)
}

async fn finished(status: &mut watch::Receiver<Option<String>>) {
    tokio::time::timeout(Duration::from_secs(3), status.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*status.borrow_and_update(), None);
}

#[tokio::test]
async fn resuming_waits_for_actual_playback_then_reveals_the_existing_workspace() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let server = serve_player(&bus, &state).await;
    let (task, states, mut status, requests) = run_press(
        &bus,
        playback(media::Playback::Paused, None),
        server.unique_name().unwrap().as_str(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(state.lock().unwrap().activated.is_none());
    assert!(!status.has_changed().unwrap());
    states.send_replace(playback(
        media::Playback::Playing,
        Some(media::Playback::Paused),
    ));
    finished(&mut status).await;
    drop(requests);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.activated, Some(2));
    assert_eq!(state.workspace, 4);
    assert_eq!(state.raises, 1);
    assert_eq!(state.toggles, 0);
}

#[tokio::test]
async fn pausing_keeps_focus_even_with_a_delayed_update_and_later_mouse_resume() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let _server = serve_player(&bus, &state).await;
    let (task, states, mut status, requests) = run_press(
        &bus,
        playback(media::Playback::Playing, None),
        "org.mpris.MediaPlayer2.vlc",
    )
    .await;
    finished(&mut status).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    states.send_replace(playback(
        media::Playback::Paused,
        Some(media::Playback::Playing),
    ));
    states.send_replace(playback(
        media::Playback::Playing,
        Some(media::Playback::Paused),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(requests);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert!(state.activated.is_none());
    assert_eq!(state.workspace, 0);
    assert_eq!(state.raises, 0);
    assert_eq!(state.toggles, 0);
}

#[tokio::test]
async fn state_changes_received_just_before_the_input_action_keep_the_toggle_direction() {
    for (now, before, reveal) in [
        (media::Playback::Paused, media::Playback::Playing, false),
        (media::Playback::Playing, media::Playback::Paused, true),
    ] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let _server = serve_player(&bus, &state).await;
        let pressed = Instant::now();
        let initial = playback(now, Some(before));
        // Model an input action queued behind other main-loop work while the
        // independent media watcher has already received the state signal.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (task, _states, mut status, requests) =
            run_press_at(&bus, initial, "org.mpris.MediaPlayer2.vlc", pressed).await;
        finished(&mut status).await;
        drop(requests);
        task.await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.activated.is_some(), reveal);
        assert_eq!(state.raises, usize::from(reveal));
        assert_eq!(state.toggles, 0);
    }
}

#[tokio::test]
async fn pausing_just_after_a_mouse_start_does_not_reveal_the_video() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let _server = serve_player(&bus, &state).await;
    let (task, _states, mut status, requests) = run_press(
        &bus,
        playback(media::Playback::Playing, Some(media::Playback::Paused)),
        "org.mpris.MediaPlayer2.vlc",
    )
    .await;
    finished(&mut status).await;
    drop(requests);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert!(state.activated.is_none());
    assert_eq!(state.raises, 0);
    assert_eq!(state.toggles, 0);
}

#[tokio::test]
async fn unknown_or_unconfirmed_playback_never_moves_focus() {
    for initial in [media::Playback::Unavailable, media::Playback::Paused] {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let _server = serve_player(&bus, &state).await;
        let (task, _states, mut status, requests) =
            run_press(&bus, playback(initial, None), "org.mpris.MediaPlayer2.vlc").await;
        finished(&mut status).await;
        drop(requests);
        task.await.unwrap();
        let state = state.lock().unwrap();
        assert!(state.activated.is_none());
        assert_eq!(state.raises, 0);
        assert_eq!(state.toggles, 0);
    }
}
#[tokio::test]
async fn reveal_activates_the_existing_video_workspace_without_a_second_toggle() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let _server = Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.mpris.MediaPlayer2.vlc")
        .unwrap()
        .name("org.gnome.Shell")
        .unwrap()
        .serve_at(media::PLAYER_PATH, Root(state.clone()))
        .unwrap()
        .serve_at(media::PLAYER_PATH, Player(state.clone()))
        .unwrap()
        .serve_at(
            "/org/gnome/Shell/Extensions/Windows",
            Windows(state.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    media_focus::reveal(
        &bus.connection().await,
        &browser::Broker::default(),
        "org.mpris.MediaPlayer2.vlc",
    )
    .await
    .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.activated, Some(2));
    assert_eq!(state.workspace, 4);
    assert_eq!(state.raises, 1);
    assert_eq!(state.toggles, 0);
}
#[tokio::test]
async fn native_host_round_trips_find_and_activate_then_disconnects_cleanly() {
    let bus = Bus::new();
    let server = bus.connection().await;
    server.request_name(sie::BUS_NAME).await.unwrap();
    let broker = Arc::new(browser::Broker::default());
    browser::export(&server, broker.clone()).await.unwrap();
    let mut host = AsyncCommand::new(env!("CARGO_BIN_EXE_sie-browser-host"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = host.stdin.take().unwrap();
    let mut output = host.stdout.take().unwrap();
    let proxy = zbus::fdo::PropertiesProxy::builder(&server)
        .destination(sie::BUS_NAME)
        .unwrap()
        .path(browser::PATH)
        .unwrap()
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let value = proxy
                .get(browser::INTERFACE.try_into().unwrap(), "ConnectedBrowsers")
                .await
                .unwrap();
            if Vec::<u32>::try_from(value)
                .unwrap()
                .contains(&std::process::id())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let active = broker.clone();
    let request =
        tokio::spawn(async move { active.activate(std::process::id(), "Video", "").await });
    let find = tokio::time::timeout(Duration::from_secs(2), browser::read_frame(&mut output))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(find["op"], "find");
    let tab = serde_json::json!({"id":7,"title":"Video - YouTube","url":"https://example.invalid/video","audible":false});
    browser::write_frame(
        &mut input,
        &serde_json::json!({"request_id":find["request_id"],"ok":true,"tabs":[tab.clone()]}),
    )
    .await
    .unwrap();
    let activate = tokio::time::timeout(Duration::from_secs(2), browser::read_frame(&mut output))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(activate["op"], "activate");
    assert_eq!(activate["tab_id"], 7);
    browser::write_frame(
        &mut input,
        &serde_json::json!({"request_id":activate["request_id"],"ok":true,"tab":tab}),
    )
    .await
    .unwrap();
    assert_eq!(request.await.unwrap().unwrap().id, 7);
    let active = broker.clone();
    let ambiguous =
        tokio::spawn(async move { active.activate(std::process::id(), "Video", "").await });
    let find = browser::read_frame(&mut output).await.unwrap().unwrap();
    let first = serde_json::json!({"id":7,"title":"Video - YouTube","url":"https://example.invalid/video","audible":false});
    let second = serde_json::json!({"id":8,"title":"Video - YouTube","url":"https://example.invalid/video","audible":false});
    browser::write_frame(
        &mut input,
        &serde_json::json!({"request_id":find["request_id"],"ok":true,"tabs":[first,second]}),
    )
    .await
    .unwrap();
    assert!(
        ambiguous
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), host.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let value = proxy
        .get(browser::INTERFACE.try_into().unwrap(), "ConnectedBrowsers")
        .await
        .unwrap();
    assert!(Vec::<u32>::try_from(value).unwrap().is_empty());
}
