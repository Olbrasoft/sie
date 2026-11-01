use super::*;

#[tokio::test]
async fn desktop_monitor_context_uses_fresh_focus_tracker_values() {
    let bus = Bus::new();
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
