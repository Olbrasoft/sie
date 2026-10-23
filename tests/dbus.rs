//! Real D-Bus round trips on a private bus; never touch the logged-in desktop.
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use sie::{
    desktop::Desktop,
    lighting::{encode_frame, touch_typing_frame},
    openrazer,
};
use zbus::{Connection, connection::Builder, zvariant::OwnedObjectPath};

struct Bus {
    process: Child,
    address: String,
}
impl Bus {
    fn start() -> Self {
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is required for integration tests");
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
    count: i32,
    current: i32,
    keys: Vec<(u32, bool)>,
    frames: Vec<Vec<u8>>,
    customs: usize,
    starts: usize,
    stops: usize,
    fail_press: bool,
}
type Shared = Arc<Mutex<State>>;

struct Tracker(Shared);
#[zbus::interface(name = "org.olbrasoft.Desktop")]
impl Tracker {
    #[zbus(property)]
    fn total_workspaces(&self) -> i32 {
        self.0.lock().unwrap().count
    }
    #[zbus(property)]
    fn current_workspace(&self) -> i32 {
        self.0.lock().unwrap().current
    }
}

struct Mutter;
#[zbus::interface(name = "org.gnome.Mutter.RemoteDesktop")]
impl Mutter {
    fn create_session(&self) -> OwnedObjectPath {
        "/org/gnome/Mutter/RemoteDesktop/Session/test"
            .try_into()
            .unwrap()
    }
}

struct Session(Shared);
#[zbus::interface(name = "org.gnome.Mutter.RemoteDesktop.Session")]
impl Session {
    fn start(&self) {
        self.0.lock().unwrap().starts += 1;
    }
    fn stop(&self) {
        self.0.lock().unwrap().stops += 1;
    }
    fn notify_keyboard_keycode(&self, key: u32, pressed: bool) -> zbus::fdo::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.keys.push((key, pressed));
        if let Some(index) = [79, 80, 81, 75, 76]
            .iter()
            .position(|code| *code == key)
            .filter(|_| pressed)
        {
            if state.fail_press {
                return Err(zbus::fdo::Error::Failed("Injected key failure".into()));
            }
            state.current = index as i32;
        }
        Ok(())
    }
}

struct RazerManager;
#[zbus::interface(name = "razer.devices")]
impl RazerManager {
    #[zbus(name = "getDevices")]
    fn devices(&self) -> Vec<String> {
        vec!["test".into(), "other".into()]
    }
}
struct RazerDevice {
    ids: Vec<i32>,
    dimensions: Vec<i32>,
}
#[zbus::interface(name = "razer.device.misc")]
impl RazerDevice {
    #[zbus(name = "getVidPid")]
    fn ids(&self) -> Vec<i32> {
        self.ids.clone()
    }
    #[zbus(name = "getMatrixDimensions")]
    fn dimensions(&self) -> Vec<i32> {
        self.dimensions.clone()
    }
}
struct RazerLighting(Shared);
#[zbus::interface(name = "razer.device.lighting.chroma")]
impl RazerLighting {
    #[zbus(name = "setKeyRow")]
    fn row(&self, payload: Vec<u8>) {
        self.0.lock().unwrap().frames.push(payload);
    }
    #[zbus(name = "setCustom")]
    fn custom(&self) {
        self.0.lock().unwrap().customs += 1;
    }
}

async fn services(bus: &Bus, state: Shared, dimensions: Vec<i32>) -> Connection {
    Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at("/org/olbrasoft/Desktop", Tracker(state.clone()))
        .unwrap()
        .serve_at("/org/gnome/Mutter/RemoteDesktop", Mutter)
        .unwrap()
        .serve_at(
            "/org/gnome/Mutter/RemoteDesktop/Session/test",
            Session(state.clone()),
        )
        .unwrap()
        .serve_at("/org/razer", RazerManager)
        .unwrap()
        .serve_at(
            "/org/razer/device/test",
            RazerDevice {
                ids: vec![0x1532, 0x028D],
                dimensions,
            },
        )
        .unwrap()
        .serve_at("/org/razer/device/test", RazerLighting(state.clone()))
        .unwrap()
        // No lighting interface: any accidental write to this model must fail the test.
        .serve_at(
            "/org/razer/device/other",
            RazerDevice {
                ids: vec![0x1532, 0x0293],
                dimensions: vec![],
            },
        )
        .unwrap()
        .name("org.olbrasoft.Desktop")
        .unwrap()
        .name("org.gnome.Mutter.RemoteDesktop")
        .unwrap()
        .name("org.razer")
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn schemas() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/schemas/org.gnome.desktop.wm.keybindings.gschema.xml"
        ),
        directory
            .path()
            .join("org.gnome.desktop.wm.keybindings.gschema.xml"),
    )
    .unwrap();
    assert!(
        Command::new("glib-compile-schemas")
            .arg(directory.path())
            .status()
            .unwrap()
            .success()
    );
    directory
}

async fn switch(bus: &Bus, schemas: &tempfile::TempDir, index: u32) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_sie"))
        .args(["--switch-workspace", &index.to_string()])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("GSETTINGS_SCHEMA_DIR", schemas.path())
        .env("GSETTINGS_BACKEND", "memory")
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn lighting_writes_a_byte_array_only_to_the_supported_model() {
    let bus = Bus::start();
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
    let bus = Bus::start();
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
async fn unavailable_workspaces_read_the_live_count_and_emit_no_input() {
    let bus = Bus::start();
    let state = Arc::new(Mutex::new(State {
        count: 4,
        current: 3,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let desktop = Desktop::connect(&bus.connection().await).await.unwrap();
    assert!(!desktop.switch(4).await.unwrap());
    state.lock().unwrap().count = 3;
    assert!(!desktop.switch(3).await.unwrap());
    assert!(desktop.switch(5).await.is_err());
    desktop.stop().await.unwrap();
    let state = state.lock().unwrap();
    assert!(state.keys.is_empty());
    assert_eq!(state.current, 3);
    assert_eq!((state.starts, state.stops), (1, 1));
}

#[tokio::test]
async fn binary_switches_all_five_targets_and_releases_every_key() {
    let bus = Bus::start();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let schemas = schemas();
    for index in 0..5 {
        let output = switch(&bus, &schemas, index).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(state.lock().unwrap().current, index as i32);
    }
    let state = state.lock().unwrap();
    assert_eq!((state.starts, state.stops), (5, 5));
    for (index, chord) in state.keys.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *chord,
            [
                (125, true),
                ([79, 80, 81, 75, 76][index], true),
                ([79, 80, 81, 75, 76][index], false),
                (125, false)
            ]
        );
    }
}

#[tokio::test]
async fn failed_key_press_still_releases_both_keys_and_stops_the_session() {
    let bus = Bus::start();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        fail_press: true,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let output = switch(&bus, &schemas(), 0).await;
    assert!(!output.status.success());
    let state = state.lock().unwrap();
    assert_eq!(
        state.keys,
        [(125, true), (79, true), (79, false), (125, false)]
    );
    assert_eq!(state.stops, 1);
}

#[tokio::test]
async fn duplicate_instance_cannot_replace_the_running_application() {
    let bus = Bus::start();
    let first = bus.connection().await;
    let second = bus.connection().await;
    sie::claim_instance(&first).await.unwrap();
    assert!(sie::claim_instance(&second).await.is_err());
    // The first connection retains ownership after the rejected attempt.
    let proxy = zbus::fdo::DBusProxy::new(&first).await.unwrap();
    assert_eq!(
        proxy
            .get_name_owner(sie::BUS_NAME.try_into().unwrap())
            .await
            .unwrap()
            .as_str(),
        first.unique_name().unwrap().as_str()
    );
}

#[tokio::test]
async fn externally_changed_workspace_moves_the_led_without_sending_keyboard_input() {
    use sie::desktop::{watch_workspaces, workspace_state};
    use std::time::Duration;

    let bus = Bus::start();
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
    let bus = Bus::start();
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
