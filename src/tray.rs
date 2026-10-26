use ksni::{MenuItem, Tray, menu::StandardItem};
use tokio::sync::mpsc;

use crate::{
    APP_NAME, VERSION,
    desktop::WorkspaceState,
    dictation::{Command as DictationCommand, Phase, Status as DictationStatus},
    lighting::LightingMode,
};

pub fn workspace_label(state: WorkspaceState) -> String {
    if state.index < 0 || state.index >= state.count {
        return "Aktivní plocha není dostupná".into();
    }
    let number = state.index + 1;
    if state.active_macro().is_some() {
        format!("Plocha {number} z {} · M{number}", state.count)
    } else {
        format!("Plocha {number} z {}", state.count)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    SetLightingMode(LightingMode),
    Dictation(DictationCommand),
    About,
    Quit,
}

pub struct AppTray {
    pub actions: mpsc::Sender<Action>,
    pub lighting_status: String,
    pub mouse_lighting_status: String,
    pub lighting_mode: LightingMode,
    pub lighting_controls_available: bool,
    pub workspace_status: String,
    pub media_status: String,
    pub focus_status: Option<String>,
    pub dictation_status: DictationStatus,
    pub dictation_controls_available: bool,
}

impl Tray for AppTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "sie".into()
    }
    fn title(&self) -> String {
        APP_NAME.into()
    }
    fn icon_name(&self) -> String {
        if self.dictation_status.phase == Phase::Recording {
            "audio-input-microphone-symbolic"
        } else {
            "input-keyboard-symbolic"
        }
        .into()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = vec![
            StandardItem {
                label: format!("Klávesnice: {}", self.lighting_status),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.mouse_lighting_status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.workspace_status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.media_status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.dictation_status.message.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: match self.lighting_mode {
                    LightingMode::On => "✅ Vypnout osvětlení klávesnice a myši",
                    LightingMode::Off => "❌ Zapnout osvětlení klávesnice a myši",
                }
                .into(),
                enabled: self.lighting_controls_available,
                activate: Box::new(|this: &mut Self| {
                    let _ = this
                        .actions
                        .try_send(Action::SetLightingMode(this.lighting_mode.toggled()));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: if self.dictation_status.enabled {
                    "✅ Diktování — Vypnout"
                } else {
                    "❌ Diktování — Zapnout"
                }
                .into(),
                enabled: self.dictation_controls_available,
                activate: Box::new(|this: &mut Self| {
                    let _ = this
                        .actions
                        .try_send(Action::Dictation(DictationCommand::SetEnabled(
                            !this.dictation_status.enabled,
                        )));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.dictation_status.phase == Phase::Recording {
                    "Ukončit a přepsat diktování"
                } else {
                    "Zahájit diktování"
                }
                .into(),
                enabled: self.dictation_controls_available
                    && self.dictation_status.enabled
                    && self.dictation_status.phase != Phase::Transcribing,
                activate: Box::new(|this: &mut Self| {
                    let _ = this
                        .actions
                        .try_send(Action::Dictation(DictationCommand::Toggle));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Zrušit diktování".into(),
                enabled: self.dictation_controls_available
                    && matches!(
                        self.dictation_status.phase,
                        Phase::Recording | Phase::Transcribing
                    ),
                activate: Box::new(|this: &mut Self| {
                    let _ = this
                        .actions
                        .try_send(Action::Dictation(DictationCommand::Cancel));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: format!("O aplikaci — {VERSION}"),
                icon_name: "help-about-symbolic".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.actions.try_send(Action::About);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Ukončit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.actions.try_send(Action::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(label) = &self.focus_status {
            menu.insert(
                0,
                StandardItem {
                    label: label.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        menu
    }
}

pub async fn show_about() -> anyhow::Result<()> {
    let status = tokio::process::Command::new("zenity")
        .args(["--info", "--no-wrap", "--title", "O aplikaci", "--text",
            &format!("{APP_NAME}\nVerze {VERSION}\n\nRozšíření ovládání počítače\nLicence MIT\n© 2026 Jiří Tůma / Olbrasoft")])
        .kill_on_drop(true).status().await?;
    anyhow::ensure!(status.success(), "About dialog exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item<'a>(items: &'a [MenuItem<AppTray>], label: &str) -> &'a StandardItem<AppTray> {
        items
            .iter()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label.contains(label) => Some(item),
                _ => None,
            })
            .expect("Menu item must exist")
    }

    #[test]
    fn workspace_labels_use_human_numbering_and_only_name_existing_macro_keys() {
        assert_eq!(
            workspace_label(WorkspaceState { index: 2, count: 5 }),
            "Plocha 3 z 5 · M3"
        );
        assert_eq!(
            workspace_label(WorkspaceState { index: 5, count: 9 }),
            "Plocha 6 z 9"
        );
        assert_eq!(
            workspace_label(WorkspaceState {
                index: -1,
                count: 0
            }),
            "Aktivní plocha není dostupná"
        );
    }

    #[tokio::test]
    async fn about_and_quit_callbacks_notify_the_application() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut tray = AppTray {
            actions: tx,
            lighting_status: "Ready".into(),
            mouse_lighting_status: "Podsvícení myši aktivní".into(),
            lighting_mode: LightingMode::On,
            lighting_controls_available: true,
            workspace_status: "Ready".into(),
            media_status: "Média nedostupná".into(),
            focus_status: None,
            dictation_status: DictationStatus::new(false, Phase::Disabled),
            dictation_controls_available: true,
        };
        let menu = tray.menu();
        let about = item(&menu, "O aplikaci");
        assert!(about.label.contains(VERSION));
        (about.activate)(&mut tray);
        assert_eq!(rx.recv().await, Some(Action::About));
        let quit = item(&menu, "Ukončit");
        (quit.activate)(&mut tray);
        assert_eq!(rx.recv().await, Some(Action::Quit));
    }

    #[tokio::test]
    async fn lighting_menu_shows_state_and_requests_the_displayed_opposite_action() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut tray = AppTray {
            actions: tx,
            lighting_status: "Ready".into(),
            mouse_lighting_status: "Podsvícení myši nedostupné".into(),
            lighting_mode: LightingMode::On,
            lighting_controls_available: true,
            workspace_status: "Ready".into(),
            media_status: "Média nedostupná".into(),
            focus_status: None,
            dictation_status: DictationStatus::new(false, Phase::Disabled),
            dictation_controls_available: true,
        };
        let menu = tray.menu();
        assert!(!item(&menu, "Podsvícení myši nedostupné").enabled);
        let toggle = item(&menu, "osvětlení klávesnice");
        assert_eq!(toggle.label, "✅ Vypnout osvětlení klávesnice a myši");
        assert!(toggle.enabled);
        (toggle.activate)(&mut tray);
        assert_eq!(
            rx.recv().await,
            Some(Action::SetLightingMode(LightingMode::Off))
        );

        // The main loop confirms the requested mode and publishes the new menu.
        tray.lighting_mode = LightingMode::Off;
        let menu = tray.menu();
        let toggle = item(&menu, "osvětlení klávesnice");
        assert_eq!(toggle.label, "❌ Zapnout osvětlení klávesnice a myši");
        assert!(
            toggle.enabled,
            "Off must remain clickable to enable lighting again"
        );
        (toggle.activate)(&mut tray);
        assert_eq!(
            rx.recv().await,
            Some(Action::SetLightingMode(LightingMode::On))
        );
    }

    #[tokio::test]
    async fn recording_and_transcription_menu_states_offer_cancel_without_starting_another_session()
    {
        let (tx, mut rx) = mpsc::channel(4);
        let mut tray = AppTray {
            actions: tx,
            lighting_status: "Ready".into(),
            mouse_lighting_status: "Podsvícení myši aktivní".into(),
            lighting_mode: LightingMode::On,
            lighting_controls_available: true,
            workspace_status: "Ready".into(),
            media_status: "Média nedostupná".into(),
            focus_status: None,
            dictation_status: DictationStatus::new(true, Phase::Recording),
            dictation_controls_available: true,
        };
        let menu = tray.menu();
        let stop = item(&menu, "Ukončit a přepsat");
        assert!(stop.enabled);
        (stop.activate)(&mut tray);
        assert_eq!(
            rx.recv().await,
            Some(Action::Dictation(DictationCommand::Toggle))
        );
        tray.dictation_status = DictationStatus::new(true, Phase::Transcribing);
        let menu = tray.menu();
        assert!(!item(&menu, "Zahájit diktování").enabled);
        let cancel = item(&menu, "Zrušit diktování");
        assert!(cancel.enabled);
        (cancel.activate)(&mut tray);
        assert_eq!(
            rx.recv().await,
            Some(Action::Dictation(DictationCommand::Cancel))
        );
    }
}
