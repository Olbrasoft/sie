//! Private bus/native-host checks; no user browser, compositor or keyboard.
use sie::{browser, media, media_focus};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::process::Command as AsyncCommand;
use zbus::{
    Connection,
    connection::Builder,
    zvariant::{OwnedValue, Str},
};
struct Bus {
    process: Child,
    address: String,
}
impl Bus {
    fn new() -> Self {
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            process,
            address: address.trim().into(),
        }
    }
    async fn connection(&self) -> Connection {
        Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
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
