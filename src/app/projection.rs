//! Project application status into keyboard lighting and tray labels.
use sie::{
    desktop,
    lighting::{DialFeedback, LightingMode, State as LightingState},
    media::Status as MediaStatus,
    tray::AppTray,
};
use tokio::sync::watch;
use zbus::Connection;

pub(super) struct Lighting {
    mode: LightingMode,
    keyboard: watch::Sender<LightingState>,
    mouse: watch::Sender<LightingMode>,
    last_status: String,
}
impl Lighting {
    pub fn new(keyboard: watch::Sender<LightingState>, mouse: watch::Sender<LightingMode>) -> Self {
        let mode = *mouse.borrow();
        Self {
            mode,
            keyboard,
            mouse,
            last_status: String::new(),
        }
    }
    pub fn set_mode(&mut self, mode: LightingMode) {
        self.mode = mode;
        self.mouse.send_if_modified(|old| {
            if *old == mode {
                false
            } else {
                *old = mode;
                true
            }
        });
    }
    pub fn set_recording(&self, recording: bool) {
        self.keyboard.send_if_modified(|s| {
            if s.recording == recording {
                false
            } else {
                s.recording = recording;
                true
            }
        });
    }
    pub fn set_dial(&self, dial: DialFeedback) {
        self.keyboard.send_if_modified(|s| {
            if s.dial == dial {
                false
            } else {
                s.dial = dial;
                true
            }
        });
    }
    pub async fn refresh(
        &mut self,
        connection: &Connection,
        tray: &ksni::Handle<AppTray>,
        media: &MediaStatus,
        recording: bool,
    ) {
        let state = desktop::workspace_state(connection).await.ok();
        let active = state.and_then(|state| state.active_macro());
        let mode = self.mode;
        let next = LightingState {
            mode,
            workspace: active,
            playback: media.playback,
            recording,
            dial: self.keyboard.borrow().dial,
        };
        self.keyboard.send_if_modified(|old| {
            if *old == next {
                return false;
            }
            *old = next;
            true
        });
        let workspace = state
            .map(sie::tray::workspace_label)
            .unwrap_or_else(|| "Ovládání ploch nedostupné".into());
        let media = media.label();
        let status = format!("{mode:?}; {workspace}; {media}");
        if status != self.last_status {
            tracing::info!("{status}");
            self.last_status = status;
            tray.update(move |tray| {
                tray.lighting_mode = mode;
                tray.workspace_status = workspace;
                tray.media_status = media;
            })
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_status_updates_preserve_workspace_media_and_other_lighting_fields() {
        let initial = LightingState {
            workspace: Some(3),
            playback: sie::media::Playback::Playing,
            recording: true,
            dial: DialFeedback::Global,
            ..Default::default()
        };
        let (keyboard, state) = watch::channel(initial);
        let (mouse, mode) = watch::channel(LightingMode::On);
        let mut lighting = Lighting::new(keyboard, mouse);
        lighting.set_mode(LightingMode::Off);
        lighting.set_recording(false);
        lighting.set_dial(DialFeedback::Local);
        assert_eq!(*mode.borrow(), LightingMode::Off);
        assert_eq!(
            *state.borrow(),
            LightingState {
                recording: false,
                dial: DialFeedback::Local,
                ..initial
            }
        );
        lighting.set_mode(LightingMode::On);
        assert_eq!(state.borrow().workspace, Some(3));
        assert_eq!(state.borrow().playback, sie::media::Playback::Playing);
    }
}
