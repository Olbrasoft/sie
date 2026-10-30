//! Real D-Bus round trips on a private bus; never touch the logged-in desktop.
#[path = "support/bus.rs"]
mod private_bus;
use private_bus::Bus;
use std::{
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zbus::connection::Builder;

use sie::{
    desktop::Desktop,
    lighting::{encode_frame, touch_typing_frame},
    openrazer,
};
use zbus::{Connection, zvariant::OwnedObjectPath};

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
    native_local_opens: Vec<i32>,
    native_fail_open: bool,
    native_fail_move: bool,
    native_version: Option<u32>,
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
    fn version(&self) -> u32 {
        self.0.lock().unwrap().native_version.unwrap_or(3)
    }
    fn open(&self) -> u32 {
        let mut state = self.0.lock().unwrap();
        state.native_active = true;
        state.native_opens += 1;
        1
    }
    fn open_local(&self, delta: i32) -> zbus::fdo::Result<u32> {
        let mut state = self.0.lock().unwrap();
        if state.native_fail_open {
            return Err(zbus::fdo::Error::Failed("No local applications".into()));
        }
        state.native_active = true;
        state.native_local_opens.push(delta);
        Ok(1)
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

#[path = "dbus/context.rs"]
mod context;
#[path = "dbus/delivery.rs"]
mod delivery;
#[path = "dbus/lighting.rs"]
mod lighting;
#[path = "dbus/switcher.rs"]
mod switcher;
#[path = "dbus/workspaces.rs"]
mod workspaces;
