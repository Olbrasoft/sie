use ksni::{MenuItem, Tray, menu::StandardItem};
use tokio::sync::mpsc;

use crate::{APP_NAME, VERSION, desktop::WorkspaceState};

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
    About,
    Quit,
}

pub struct AppTray {
    pub actions: mpsc::Sender<Action>,
    pub lighting_status: String,
    pub workspace_status: String,
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
        "input-keyboard-symbolic".into()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: self.lighting_status.clone(),
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
        ]
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
            workspace_status: "Ready".into(),
        };
        let menu = tray.menu();
        let MenuItem::Standard(about) = &menu[3] else {
            panic!("About must be an action")
        };
        assert!(about.label.contains(VERSION));
        (about.activate)(&mut tray);
        assert_eq!(rx.recv().await, Some(Action::About));
        let MenuItem::Standard(quit) = &menu[4] else {
            panic!("Quit must be an action")
        };
        (quit.activate)(&mut tray);
        assert_eq!(rx.recv().await, Some(Action::Quit));
    }
}
