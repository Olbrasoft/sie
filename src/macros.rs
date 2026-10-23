use evdev::{EventType, InputEvent, KeyCode};

// Linux input-event-codes.h; evdev 0.13 does not name these newer constants.
pub const MACRO1: u16 = 0x290;
const MACRO5: u16 = 0x294;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Workspace(u32),
    DictationToggle,
    DictationCancel,
}

pub fn action_for_event(event: InputEvent) -> Option<InputAction> {
    if let Some(index) = workspace_for_event(event) {
        return Some(InputAction::Workspace(index));
    }
    if event.event_type() != EventType::KEY || event.value() != 0 {
        return None;
    }
    match event.code() {
        code if code == KeyCode::KEY_F19.code() || code == MACRO1 + 6 => {
            Some(InputAction::DictationCancel)
        }
        code if code == KeyCode::KEY_F20.code() || code == MACRO1 + 7 => {
            Some(InputAction::DictationToggle)
        }
        _ => None,
    }
}

/// OpenRazer emits F13–F17; newer input mappings may emit KEY_MACRO1–5.
pub fn workspace_for_event(event: InputEvent) -> Option<u32> {
    if event.event_type() != EventType::KEY || event.value() != 1 {
        return None;
    }
    match event.code() {
        code if (KeyCode::KEY_F13.code()..=KeyCode::KEY_F17.code()).contains(&code) => {
            Some(u32::from(code - KeyCode::KEY_F13.code()))
        }
        code if (MACRO1..=MACRO5).contains(&code) => Some(u32::from(code - MACRO1)),
        _ => None,
    }
}

pub fn workspace_exists(index: u32, count: i32) -> bool {
    index < 5 && count > 0 && index < count as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_buttons_use_release_events_without_reusing_system_mute_or_legacy_keys() {
        for code in [KeyCode::KEY_F20.code(), MACRO1 + 7] {
            assert_eq!(
                action_for_event(InputEvent::new(1, code, 0)),
                Some(InputAction::DictationToggle)
            );
            assert_eq!(action_for_event(InputEvent::new(1, code, 1)), None);
            assert_eq!(action_for_event(InputEvent::new(1, code, 2)), None);
        }
        for code in [KeyCode::KEY_F19.code(), MACRO1 + 6] {
            assert_eq!(
                action_for_event(InputEvent::new(1, code, 0)),
                Some(InputAction::DictationCancel)
            );
        }
        for code in [
            KeyCode::KEY_F18,
            KeyCode::KEY_SCROLLLOCK,
            KeyCode::KEY_PAUSE,
            KeyCode::KEY_A,
        ] {
            assert_eq!(action_for_event(InputEvent::new(1, code.code(), 0)), None);
        }
    }

    #[test]
    fn both_driver_mappings_cover_all_five_macro_keys() {
        for start in [KeyCode::KEY_F13.code(), MACRO1] {
            for i in 0..5 {
                assert_eq!(
                    workspace_for_event(InputEvent::new(EventType::KEY.0, start + i, 1)),
                    Some(u32::from(i))
                );
            }
        }
    }

    #[test]
    fn normal_typing_releases_repeats_and_non_key_events_are_ignored() {
        for value in [0, 2, -1] {
            assert_eq!(
                workspace_for_event(InputEvent::new(
                    EventType::KEY.0,
                    KeyCode::KEY_F13.code(),
                    value
                )),
                None
            );
        }
        for code in [
            KeyCode::KEY_A,
            KeyCode::KEY_F12,
            KeyCode::KEY_F18,
            KeyCode::new(MACRO5 + 1),
        ] {
            assert_eq!(
                workspace_for_event(InputEvent::new(EventType::KEY.0, code.code(), 1)),
                None
            );
        }
        assert_eq!(
            workspace_for_event(InputEvent::new(
                EventType::MISC.0,
                KeyCode::KEY_F13.code(),
                1
            )),
            None
        );
    }

    #[test]
    fn targets_depend_on_the_live_count_without_creating_workspaces() {
        for count in -1..=8 {
            for index in 0..=6 {
                assert_eq!(
                    workspace_exists(index, count),
                    count > 0 && index < 5 && index < count as u32
                );
            }
        }
        assert!(workspace_exists(3, 4));
        assert!(!workspace_exists(4, 4));
        assert!(workspace_exists(4, 5));
        assert!(!workspace_exists(u32::MAX, 5));
    }
}
