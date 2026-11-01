//! Read-only diagnostics, separate from application startup and device control.
use anyhow::Result;
use sie::{
    config::DictationConfig, corrections::Database, desktop, input, mouse_lighting, openrazer,
};
use zbus::Connection;

pub async fn run(connection: &Connection, dictation: &DictationConfig) -> Result<()> {
    let mut failures = Vec::new();
    macro_rules! check {
        ($name:expr, $result:expr) => {
            match $result {
                Ok(detail) => println!("OK   {}: {detail}", $name),
                Err(error) => {
                    println!("FAIL {}: {error:#}", $name);
                    failures.push($name);
                }
            }
        };
    }
    check!(
        "OpenRazer",
        openrazer::devices(connection).await.and_then(|paths| {
            anyhow::ensure!(!paths.is_empty(), "No supported keyboard found");
            Ok(format!("{} BlackWidow V4 Pro, matrix 8 × 23", paths.len()))
        })
    );
    check!(
        "Macro input",
        input::discover().and_then(|devices| {
            anyhow::ensure!(
                !devices.is_empty(),
                "No readable supported keyboard input; check /dev/input permissions"
            );
            Ok(format!("{} interface(s)", devices.len()))
        })
    );
    check!(
        "Mouse lighting",
        tokio::task::spawn_blocking(mouse_lighting::check_available).await?
    );
    check!(
        "Workspace tracker",
        desktop::workspace_count(connection)
            .await
            .map(|n| format!("{n} existing workspaces"))
    );
    for index in 0..5 {
        check!(
            "Workspace shortcut",
            desktop::check_binding(index)
                .await
                .map(|()| format!("M{} → workspace {index}", index + 1))
        );
    }
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    check!(
        "Tray host",
        dbus.name_has_owner("org.kde.StatusNotifierWatcher".try_into()?)
            .await
            .and_then(|owned| {
                if owned {
                    Ok("StatusNotifierWatcher available")
                } else {
                    Err(zbus::fdo::Error::Failed(
                        "Enable GNOME AppIndicator support".into(),
                    ))
                }
            })
    );
    check!(
        "Mutter input",
        zbus::Proxy::new(
            connection,
            "org.gnome.Mutter.RemoteDesktop",
            "/org/gnome/Mutter/RemoteDesktop",
            "org.gnome.Mutter.RemoteDesktop"
        )
        .await?
        .get_property::<i32>("Version")
        .await
        .map(|v| format!("API version {v}"))
    );
    check!(
        "About dialog",
        tokio::process::Command::new("zenity")
            .arg("--version")
            .output()
            .await
            .and_then(|out| {
                if out.status.success() {
                    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
                } else {
                    Err(std::io::Error::other("zenity unavailable"))
                }
            })
    );
    if dictation.enabled {
        check!(
            "Whisper model",
            if dictation.model_path.is_file() {
                Ok(format!(
                    "local model; CUDA build: {}; Flash Attention: {}",
                    cfg!(feature = "cuda"),
                    dictation.flash_attention && dictation.use_gpu && cfg!(feature = "cuda")
                ))
            } else {
                Err(anyhow::anyhow!("Local model is missing"))
            }
        );
        check!(
            "Desktop context",
            sie::context::snapshot(connection)
                .await
                .map(|context| context.application)
        );
        for (label, operation, device) in [
            (
                "Dictation microphone",
                "get-source-mute",
                dictation
                    .audio_source
                    .as_deref()
                    .unwrap_or("@DEFAULT_SOURCE@"),
            ),
            (
                "Dictation sound output",
                "get-sink-mute",
                dictation.sound_sink.as_deref().unwrap_or("@DEFAULT_SINK@"),
            ),
        ] {
            check!(
                label,
                async {
                    let output = tokio::process::Command::new("pactl")
                        .args([operation, device])
                        .env("LC_ALL", "C")
                        .output()
                        .await?;
                    anyhow::ensure!(
                        output.status.success(),
                        "Configured audio device is unavailable"
                    );
                    anyhow::Ok(format!(
                        "{} ({})",
                        device,
                        String::from_utf8_lossy(&output.stdout).trim()
                    ))
                }
                .await
            );
        }
        check!(
            "Dictation database",
            async {
                Database::connect(&dictation.database_url)
                    .await?
                    .postprocessor()
                    .await?;
                anyhow::Ok("independent correction database available")
            }
            .await
        );
    }
    anyhow::ensure!(
        failures.is_empty(),
        "Unavailable integrations: {}",
        failures.join(", ")
    );
    Ok(())
}
