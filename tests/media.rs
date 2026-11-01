//! Private session bus only: no real players, keyboard or media control.
#[path = "support/bus.rs"]
mod private_bus;
use private_bus::Bus;
use sie::{
    lighting::LightingMode,
    media::{self, Playback, Status},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use zbus::connection::Builder;
use zbus::{
    Connection,
    zvariant::{OwnedValue, Str},
};

#[derive(Clone)]
struct PlayerData {
    status: String,
    controllable: bool,
    calls: usize,
}
struct Player(Arc<Mutex<PlayerData>>);
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.0.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        self.0.lock().unwrap().controllable
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.0.lock().unwrap().controllable
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.0.lock().unwrap().controllable
    }
    fn play_pause(&self) {
        self.0.lock().unwrap().calls += 1;
    }
}
struct Root(&'static str);
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn identity(&self) -> &str {
        self.0
    }
}
async fn player(
    bus: &Bus,
    name: &str,
    identity: &'static str,
    initial: &str,
) -> (Connection, Arc<Mutex<PlayerData>>) {
    let data = Arc::new(Mutex::new(PlayerData {
        status: initial.into(),
        controllable: true,
        calls: 0,
    }));
    let connection = Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(media::PLAYER_PATH, Player(data.clone()))
        .unwrap()
        .serve_at(media::PLAYER_PATH, Root(identity))
        .unwrap()
        .name(name)
        .unwrap()
        .build()
        .await
        .unwrap();
    (connection, data)
}
async fn change(connection: &Connection, data: &Arc<Mutex<PlayerData>>, state: &str) {
    data.lock().unwrap().status = state.into();
    let values =
        std::collections::HashMap::from([("PlaybackStatus", OwnedValue::from(Str::from(state)))]);
    connection
        .emit_signal(
            None::<&str>,
            media::PLAYER_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(media::PLAYER_INTERFACE, values, Vec::<String>::new()),
        )
        .await
        .unwrap();
}
async fn wait(receiver: &mut watch::Receiver<Status>, expected: Playback, identity: Option<&str>) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = receiver.borrow_and_update().clone();
            if current.playback == expected && current.player.as_deref() == identity {
                break;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn background_changes_survive_workspace_focus_and_player_removal_without_sending_controls() {
    let bus = Bus::new();
    let name = "org.mpris.MediaPlayer2.chrome";
    let (chrome, chrome_data) = player(&bus, name, "Chrome", "Paused").await;
    let (status, mut receiver) = watch::channel(Status::default());
    let (_targets, target_rx) = mpsc::channel(8);
    let task = tokio::spawn(media::watch(bus.connection().await, status, target_rx));
    wait(&mut receiver, Playback::Paused, Some("Chrome")).await;
    // A property signal represents Space, mouse, remote control or playback
    // on another workspace: no keyboard event or focused window is involved.
    change(&chrome, &chrome_data, "Playing").await;
    wait(&mut receiver, Playback::Playing, Some("Chrome")).await;
    assert_eq!(
        LightingMode::On.frame_with_media(Some(0), receiver.borrow().playback)[0][20],
        [0, 255, 0]
    );
    assert!(
        LightingMode::Off
            .frame_with_media(Some(4), receiver.borrow().playback)
            .iter()
            .flatten()
            .all(|rgb| *rgb == [0, 0, 0])
    );
    let (vlc, vlc_data) = player(&bus, "org.mpris.MediaPlayer2.vlc", "VLC", "Paused").await;
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert_eq!(receiver.borrow().player.as_deref(), Some("Chrome"));
    change(&chrome, &chrome_data, "Paused").await;
    wait(&mut receiver, Playback::Paused, Some("Chrome")).await;
    change(&vlc, &vlc_data, "Playing").await;
    wait(&mut receiver, Playback::Playing, Some("VLC")).await;
    vlc.release_name("org.mpris.MediaPlayer2.vlc")
        .await
        .unwrap();
    wait(&mut receiver, Playback::Paused, Some("Chrome")).await;
    change(&chrome, &chrome_data, "Stopped").await;
    wait(&mut receiver, Playback::Paused, Some("Chrome")).await;
    chrome.release_name(name).await.unwrap();
    wait(&mut receiver, Playback::Unavailable, None).await;
    assert_eq!(chrome_data.lock().unwrap().calls, 0);
    assert_eq!(vlc_data.lock().unwrap().calls, 0);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn existing_playback_is_green_on_start_and_uncontrollable_players_are_white() {
    let bus = Bus::new();
    let (_player, data) = player(&bus, "org.mpris.MediaPlayer2.vlc", "VLC", "Playing").await;
    let (status, mut receiver) = watch::channel(Status::default());
    let (_targets, target_rx) = mpsc::channel(8);
    let task = tokio::spawn(media::watch(bus.connection().await, status, target_rx));
    wait(&mut receiver, Playback::Playing, Some("VLC")).await;
    data.lock().unwrap().controllable = false;
    change(&_player, &data, "Playing").await;
    wait(&mut receiver, Playback::Unavailable, Some("VLC")).await;
    assert_eq!(data.lock().unwrap().calls, 0);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn observer_identifies_only_native_media_key_calls_without_duplicating_playback() {
    let bus = Bus::new();
    let name = "org.mpris.MediaPlayer2.vlc";
    let (_player, data) = player(&bus, name, "VLC", "Paused").await;
    let controller = Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.gnome.SettingsDaemon.MediaKeys")
        .unwrap()
        .build()
        .await
        .unwrap();
    let (targets, mut target_rx) = mpsc::channel(8);
    let observer = media::NativeObserver::new(bus.connection().await)
        .await
        .unwrap();
    let observer = tokio::spawn(observer.run(targets));
    let proxy = zbus::Proxy::new(
        &controller,
        name,
        media::PLAYER_PATH,
        media::PLAYER_INTERFACE,
    )
    .await
    .unwrap();
    proxy.call::<_, _, ()>("PlayPause", &()).await.unwrap();
    let target = tokio::time::timeout(Duration::from_secs(2), target_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target, name);
    assert_eq!(data.lock().unwrap().calls, 1);
    let outsider = bus.connection().await;
    let proxy = zbus::Proxy::new(&outsider, name, media::PLAYER_PATH, media::PLAYER_INTERFACE)
        .await
        .unwrap();
    proxy.call::<_, _, ()>("PlayPause", &()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(80), target_rx.recv())
            .await
            .is_err()
    );
    assert_eq!(data.lock().unwrap().calls, 2);
    observer.abort();
    let _ = observer.await;
}
