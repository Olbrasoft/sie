//! Volatile G203 lighting worker; platform transport and RGB policy are separate.
mod protocol;
mod transport;
use crate::lighting::LightingMode;
use anyhow::Result;
use protocol::Mouse;
use std::time::Duration;
use tokio::sync::watch;
use transport::Hidraw;

fn connect() -> Result<Option<Mouse<Hidraw>>> {
    transport::connect()?.map(Mouse::discover).transpose()
}

pub fn check_available() -> Result<String> {
    match connect()? {
        Some(mut mouse) => Ok(format!(
            "Logitech G203 LIGHTSYNC; LED control mode {}; RGB power {}",
            mouse.software_mode()?,
            mouse.power_mode()?
        )),
        None => Ok("supported mouse not connected; keyboard control remains available".into()),
    }
}

#[derive(Default)]
struct Backend {
    mouse: Option<Mouse<Hidraw>>,
}

trait LightingBackend: Send + 'static {
    fn apply(&mut self, mode: LightingMode) -> Result<&'static str>;
}

impl LightingBackend for Backend {
    fn apply(&mut self, mode: LightingMode) -> Result<&'static str> {
        if self
            .mouse
            .as_ref()
            .is_some_and(|m| !m.transport.connected())
        {
            self.mouse = None;
        }
        let result = (|| {
            if self.mouse.is_none() {
                self.mouse = connect()?;
            }
            match self.mouse.as_mut() {
                Some(mouse) => {
                    mouse.apply(mode)?;
                    Ok(if mode == LightingMode::Off {
                        "Osvětlení myši vypnuté"
                    } else {
                        "Podsvícení myši aktivní"
                    })
                }
                None => Ok("Myš není připojena"),
            }
        })();
        if result.is_err() {
            self.mouse = None;
        }
        result
    }
}

/// A separate bounded worker handles only mode changes and two-second reconnect checks.
pub async fn run(requests: watch::Receiver<LightingMode>, status: watch::Sender<String>) {
    run_backend(requests, status, Backend::default(), Duration::from_secs(2)).await;
}

async fn run_backend<B: LightingBackend>(
    mut requests: watch::Receiver<LightingMode>,
    status: watch::Sender<String>,
    mut backend: B,
    interval: Duration,
) {
    let mut retry = tokio::time::interval(interval);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retry.tick().await;
    let mut last_error = String::new();
    loop {
        let mode = *requests.borrow_and_update();
        let result = tokio::task::spawn_blocking(move || {
            let result = backend.apply(mode);
            (backend, result)
        })
        .await;
        let label = match result {
            Ok((next, result)) => {
                backend = next;
                match result {
                    Ok(label) => {
                        last_error.clear();
                        label
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        if last_error != message {
                            tracing::warn!(%message, "Mouse lighting unavailable");
                            last_error = message;
                        }
                        "Podsvícení myši nedostupné"
                    }
                }
            }
            Err(error) => {
                tracing::error!(%error, "Mouse lighting worker stopped");
                status.send_replace("Podsvícení myši nedostupné".into());
                return;
            }
        };
        status.send_if_modified(|old| {
            if old == label {
                false
            } else {
                *old = label.into();
                true
            }
        });
        tokio::select! {
            changed = requests.changed() => if changed.is_err() { break; },
            _ = retry.tick() => {},
        }
    }
    // Like the keyboard, keep the selected lighting state on Quit, including off.
}

#[cfg(test)]
mod tests;
