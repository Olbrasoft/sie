//! Real D-Bus round trips on a private bus; never touch the logged-in desktop.
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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
    fail_key: Option<u32>,
    active_application: String,
    active_window: String,
    focused_window: u32,
    switch_on_paste: bool,
    cancel_on_paste: Option<Arc<AtomicBool>>,
    switch_on_start: bool,
    missing_keyboard: bool,
    switcher_windows: Option<Vec<serde_json::Value>>,
    activated_windows: Vec<u32>,
    native_active: bool,
    native_steps: Vec<i32>,
    native_finishes: Vec<bool>,
    native_opens: usize,
    native_fail_move: bool,
}
type Shared = Arc<Mutex<State>>;

struct Tracker(Shared);
#[zbus::interface(name = "org.olbrasoft.Desktop")]
impl Tracker {
    #[zbus(property)]
    fn active_application(&self) -> String {
        self.0.lock().unwrap().active_application.clone()
    }
    #[zbus(property)]
    fn active_window(&self) -> String {
        self.0.lock().unwrap().active_window.clone()
    }
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
        let mut state = self.0.lock().unwrap();
        state.starts += 1;
        if state.switch_on_start {
            state.focused_window = 2;
        }
    }
    fn stop(&self) {
        self.0.lock().unwrap().stops += 1;
    }
    fn notify_keyboard_keycode(&self, key: u32, pressed: bool) -> zbus::fdo::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.keys.push((key, pressed));
        if key == 47 && pressed {
            if state.switch_on_paste {
                state.focused_window = 2;
            }
            if let Some(cancelled) = &state.cancel_on_paste {
                cancelled.store(true, Ordering::SeqCst);
            }
        }
        if pressed && state.fail_key == Some(key) {
            return Err(zbus::fdo::Error::Failed("Injected key failure".into()));
        }
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

struct Windows(Shared);
#[zbus::interface(name = "org.gnome.Shell.Extensions.Windows")]
impl Windows {
    fn list(&self) -> String {
        let state = self.0.lock().unwrap();
        if let Some(windows) = &state.switcher_windows {
            let mut windows = windows.clone();
            for window in &mut windows {
                window["focus"] =
                    (window["id"].as_u64() == Some(u64::from(state.focused_window))).into();
            }
            return serde_json::to_string(&windows).unwrap();
        }
        let focus = state.focused_window;
        serde_json::json!([
            {"id":1,"wm_class":"org.gnome.TextEditor","workspace":0,"focus":focus==1},
            {"id":2,"wm_class":"OtherApplication","workspace":1,"focus":focus==2}
        ])
        .to_string()
    }

    fn activate(&self, id: u32) {
        let mut state = self.0.lock().unwrap();
        state.activated_windows.push(id);
        state.focused_window = id;
        let workspace = if let Some(windows) = &state.switcher_windows {
            windows
                .iter()
                .find(|w| w["id"].as_u64() == Some(u64::from(id)))
                .and_then(|w| w["workspace"].as_i64())
                .map(|w| w as i32)
        } else {
            Some(id as i32 - 1)
        };
        if let Some(workspace) = workspace {
            state.current = workspace;
        }
    }
}

struct RazerManager(Shared);

struct NativeSwitcher(Shared);
#[zbus::interface(name = "cz.olbrasoft.SIE.Switcher")]
impl NativeSwitcher {
    fn open(&self) -> u32 {
        let mut state = self.0.lock().unwrap();
        state.native_active = true;
        state.native_opens += 1;
        1
    }
    fn active(&self, token: u32) -> bool {
        token == 1 && self.0.lock().unwrap().native_active
    }
    fn r#move(&self, token: u32, delta: i32) -> zbus::fdo::Result<bool> {
        let mut state = self.0.lock().unwrap();
        if state.native_fail_move {
            return Err(zbus::fdo::Error::Failed(
                "Injected native switcher error".into(),
            ));
        }
        if token != 1 || !state.native_active {
            return Ok(false);
        }
        state.native_steps.push(delta);
        Ok(true)
    }
    fn finish(&self, token: u32, confirm: bool) -> bool {
        let mut state = self.0.lock().unwrap();
        if token != 1 || !state.native_active {
            return false;
        }
        state.native_active = false;
        state.native_finishes.push(confirm);
        true
    }
}

#[zbus::interface(name = "razer.devices")]
impl RazerManager {
    #[zbus(name = "getDevices")]
    fn devices(&self) -> Vec<String> {
        if self.0.lock().unwrap().missing_keyboard {
            return vec![];
        }
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
        .serve_at(sie::switcher::NATIVE_PATH, NativeSwitcher(state.clone()))
        .unwrap()
        .serve_at(
            "/org/gnome/Shell/Extensions/Windows",
            Windows(state.clone()),
        )
        .unwrap()
        .serve_at("/org/gnome/Mutter/RemoteDesktop", Mutter)
        .unwrap()
        .serve_at(
            "/org/gnome/Mutter/RemoteDesktop/Session/test",
            Session(state.clone()),
        )
        .unwrap()
        .serve_at("/org/razer", RazerManager(state.clone()))
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
        .name(sie::switcher::NATIVE_BUS)
        .unwrap()
        .name("org.gnome.Mutter.RemoteDesktop")
        .unwrap()
        .name("org.razer")
        .unwrap()
        .name("org.gnome.Shell")
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

#[tokio::test]
async fn lighting_toggle_stays_dark_during_workspace_control_and_restores_the_current_profile() {
    use sie::lighting::{LightingMode, workspace_frame};

    let bus = Bus::start();
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
async fn paste_shortcut_failure_still_releases_the_character_and_modifier() {
    let bus = Bus::start();
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
async fn canceled_dictation_cannot_touch_the_clipboard_or_start_an_input_session() {
    use std::sync::atomic::AtomicBool;
    let bus = Bus::start();
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
async fn desktop_monitor_context_uses_fresh_focus_tracker_values() {
    let bus = Bus::start();
    let state = Shared::default();
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let connection = bus.connection().await;
    {
        let mut state = state.lock().unwrap();
        state.active_application = "Alacritty.desktop".into();
        state.active_window = "Codex".into();
    }
    let first = sie::context::snapshot(&connection).await.unwrap();
    assert_eq!(first.application, "Alacritty.desktop");
    assert_eq!(first.window_title, "Codex");
    {
        let mut state = state.lock().unwrap();
        state.active_application = "org.gnome.TextEditor.desktop".into();
        state.active_window = "Český dokument".into();
    }
    let second = sie::context::snapshot(&connection).await.unwrap();
    assert_eq!(second.application, "org.gnome.TextEditor.desktop");
    assert_eq!(second.window_title, "Český dokument");
}

#[tokio::test]
async fn submit_sends_one_enter_after_each_successful_clipboard_paste_path() {
    use sie::desktop::{DeliveryMode, FocusTarget};
    for keys in [&[29, 47][..], &[29, 42, 47][..], &[42, 110][..]] {
        let bus = Bus::start();
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
        let bus = Bus::start();
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
        let bus = Bus::start();
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
        let bus = Bus::start();
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
    let bus = Bus::start();
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

async fn wait_for_frames(state: &Shared, minimum: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while state.lock().unwrap().frames.len() < minimum {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_native(state: &Shared, predicate: impl Fn(&State) -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !predicate(&state.lock().unwrap()) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_dial_click_turn_confirm_never_injects_keyboard_modifiers_or_opens_a_view() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    let bus = Bus::start();
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
    let bus = Bus::start();
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
    let bus = Bus::start();
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
async fn missing_native_extension_leaves_local_rotation_available_without_synthetic_keys() {
    use sie::{
        lighting::DialFeedback,
        switcher::{self, Command},
    };
    use tokio::sync::{mpsc, watch};
    let bus = Bus::start();
    let state = Arc::new(Mutex::new(State {
        focused_window: 1,
        switcher_windows: Some(vec![
            serde_json::json!({"id":1,"wm_class":"First","workspace":0}),
            serde_json::json!({"id":2,"wm_class":"Second","workspace":0}),
        ]),
        ..Default::default()
    }));
    let server = services(&bus, state.clone(), vec![8, 23]).await;
    server.release_name(switcher::NATIVE_BUS).await.unwrap();
    let connection = bus.connection().await;
    assert!(!switcher::available(&connection).await);
    let (commands, receiver) = mpsc::channel(4);
    let (feedback, feedback_rx) = watch::channel(DialFeedback::Idle);
    let task = tokio::spawn(switcher::run(connection, receiver, feedback));
    commands.send(Command::Click).await.unwrap();
    commands.send(Command::Turn(1)).await.unwrap();
    wait_for_native(&state, |s| !s.activated_windows.is_empty()).await;
    drop(commands);
    task.await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.native_opens, 0);
    assert_eq!(state.activated_windows, [2]);
    assert!(state.keys.is_empty());
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
}

#[tokio::test]
async fn local_dial_activation_validates_workspace_and_window_before_activation() {
    let bus = Bus::start();
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
async fn local_dial_worker_cycles_only_current_windows_and_restores_white_feedback() {
    use sie::{lighting::DialFeedback, switcher};
    use tokio::sync::{mpsc, watch};
    let bus = Bus::start();
    let state = Arc::new(Mutex::new(State {
        focused_window: 1,
        switcher_windows: Some(vec![
            serde_json::json!({"id":1,"wm_class":"First","workspace":0}),
            serde_json::json!({"id":2,"wm_class":"Second","workspace":0}),
            serde_json::json!({"id":3,"wm_class":"OtherWorkspace","workspace":4}),
        ]),
        ..Default::default()
    }));
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
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        feedback_rx.wait_for(|v| *v == DialFeedback::Idle),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(state.lock().unwrap().activated_windows, [2]);
    commands.send(switcher::Command::Turn(-1)).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.lock().unwrap().activated_windows.len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    drop(commands);
    task.await.unwrap();
    assert_eq!(state.lock().unwrap().activated_windows, [2, 1]);
    assert_eq!(*feedback_rx.borrow(), DialFeedback::Idle);
    assert_eq!(state.lock().unwrap().current, 0);
}

#[tokio::test]
async fn recording_renderer_preserves_keys_handles_off_reconnect_and_restores_idle_on_shutdown() {
    use sie::{
        lighting::{LightingMode, State as LightingState},
        media::Playback,
    };
    use tokio::sync::watch;
    let bus = Bus::start();
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
