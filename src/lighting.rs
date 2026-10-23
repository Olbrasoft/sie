//! Native BlackWidow V4 Pro matrix. Coordinates are (row, column).

pub const ROWS: usize = 8;
pub const COLS: usize = 23;
pub type Rgb = [u8; 3];
pub type Frame = [[Rgb; COLS]; ROWS];
pub const PLAY_PAUSE_KEY: (usize, usize) = (0, 20);
pub const MEDIA_PLAYING_GREEN: Rgb = [0, 255, 0];
pub const MEDIA_PAUSED_RED: Rgb = [255, 0, 0];
pub const WHITE: Rgb = [255, 255, 255];
pub const ACTIVE_WORKSPACE_GREEN: Rgb = [0, 255, 0];
// The device's macro LED rows run in reverse physical key order: M1 → M5.
pub const MACRO_KEYS: [(usize, usize); 5] = [(5, 1), (4, 1), (3, 1), (2, 1), (1, 1)];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LightingMode {
    #[default]
    On,
    Off,
}

impl LightingMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off => Self::On,
        }
    }

    /// Off takes precedence over every profile color and workspace indicator.
    pub fn frame(self, active_workspace: Option<i32>) -> Frame {
        self.frame_with_media(active_workspace, crate::media::Playback::Unavailable)
    }

    pub fn frame_with_media(
        self,
        active_workspace: Option<i32>,
        playback: crate::media::Playback,
    ) -> Frame {
        match self {
            Self::On => {
                let mut frame = workspace_frame(active_workspace);
                let (row, column) = PLAY_PAUSE_KEY;
                frame[row][column] = match playback {
                    crate::media::Playback::Unavailable => WHITE,
                    crate::media::Playback::Paused => MEDIA_PAUSED_RED,
                    crate::media::Playback::Playing => MEDIA_PLAYING_GREEN,
                };
                frame
            }
            Self::Off => [[[0, 0, 0]; COLS]; ROWS],
        }
    }
}

const RED: Rgb = [0xF0, 0x44, 0x5A];
const ORANGE: Rgb = [0xFF, 0x8A, 0x3D];
const YELLOW: Rgb = [0xF6, 0xC4, 0x53];
const GREEN: Rgb = [0x65, 0xB8, 0x4B];
const CYAN: Rgb = [0x45, 0xA5, 0xC9];
const BLUE: Rgb = [0x36, 0x57, 0xB7];
const PURPLE: Rgb = [0x76, 0x64, 0xB4];
const PINK: Rgb = [0xD0, 0x52, 0x91];

pub fn touch_typing_frame() -> Frame {
    let mut frame = [[WHITE; COLS]; ROWS];
    // Escape is x2-y0 in the native BlackWidow V4 Pro device map.
    frame[0][2] = [255, 0, 0];
    for row in &mut frame[1..=3] {
        row[3..=13].copy_from_slice(&[
            RED, ORANGE, YELLOW, GREEN, GREEN, CYAN, CYAN, BLUE, PURPLE, PINK, PINK,
        ]);
    }
    frame[1][2] = RED;
    frame[1][14] = PINK;
    frame[2][14] = PINK;
    frame[2][15] = PINK;
    frame[3][15] = [0, 255, 0];
    frame[4][4..=13].copy_from_slice(&[
        RED, ORANGE, YELLOW, GREEN, GREEN, CYAN, CYAN, BLUE, PURPLE, PINK,
    ]);
    frame[5][3] = [0xA8, 0, 0x30];
    frame
}

/// Overlay the active GNOME index (0–4) without altering touch-typing colors.
pub fn workspace_frame(active_index: Option<i32>) -> Frame {
    let mut frame = touch_typing_frame();
    if let Some(index) = active_index.filter(|index| (0..5).contains(index)) {
        let (row, column) = MACRO_KEYS[index as usize];
        frame[row][column] = ACTIVE_WORKSPACE_GREEN;
    }
    frame
}

/// OpenRazer expects row, first column, last column, followed by RGB triples.
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ROWS * (3 + COLS * 3));
    for (index, row) in frame.iter().enumerate() {
        bytes.extend_from_slice(&[index as u8, 0, (COLS - 1) as u8]);
        for rgb in row {
            bytes.extend_from_slice(rgb);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_overlay_changes_only_the_second_physical_media_key() {
        for workspace in [None, Some(0), Some(2), Some(4), Some(7)] {
            let base = workspace_frame(workspace);
            for (playback, color) in [
                (crate::media::Playback::Unavailable, WHITE),
                (crate::media::Playback::Paused, [255, 0, 0]),
                (crate::media::Playback::Playing, [0, 255, 0]),
            ] {
                let frame = LightingMode::On.frame_with_media(workspace, playback);
                for row in 0..ROWS {
                    for column in 0..COLS {
                        assert_eq!(
                            frame[row][column],
                            if (row, column) == (0, 20) {
                                color
                            } else {
                                base[row][column]
                            },
                            "cell ({row},{column})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lighting_off_overrides_media_status_and_enabling_restores_current_playback() {
        for playback in [
            crate::media::Playback::Unavailable,
            crate::media::Playback::Paused,
            crate::media::Playback::Playing,
        ] {
            let off = LightingMode::Off.frame_with_media(Some(3), playback);
            assert!(off.iter().flatten().all(|cell| *cell == [0, 0, 0]));
        }
        let restored = LightingMode::Off
            .toggled()
            .frame_with_media(Some(3), crate::media::Playback::Playing);
        assert_eq!(restored[0][20], [0, 255, 0]);
        assert_eq!(restored[2][1], [0, 255, 0]);
    }

    #[test]
    fn off_mode_keeps_keys_and_ambient_rows_black_across_workspace_changes() {
        let off = LightingMode::On.toggled();
        for workspace in [None, Some(0), Some(2), Some(3), Some(4), Some(8)] {
            let frame = off.frame(workspace);
            for (row, colors) in frame.iter().enumerate() {
                for (column, rgb) in colors.iter().enumerate() {
                    assert_eq!(*rgb, [0, 0, 0], "cell ({row}, {column}) must stay dark");
                }
            }
        }
    }

    #[test]
    fn enabling_again_restores_the_profile_with_the_current_workspace() {
        let mode = LightingMode::On.toggled();
        assert_eq!(mode.frame(Some(2))[3][1], [0, 0, 0]);
        let restored = mode.toggled().frame(Some(3));
        assert_eq!(restored, workspace_frame(Some(3)));
        assert_eq!(
            restored[3][1], WHITE,
            "The previously active M3 stays white"
        );
        assert_eq!(
            restored[2][1],
            [0, 255, 0],
            "The currently active M4 is green"
        );
        assert_eq!(
            restored[5][3],
            [0xA8, 0, 0x30],
            "Restore the system key accent"
        );
    }

    #[test]
    fn each_workspace_lights_only_its_physical_macro_key_and_preserves_all_other_cells() {
        // Coordinates independently verified against the native BlackWidow device map.
        let expected_positions = [(5, 1), (4, 1), (3, 1), (2, 1), (1, 1)];
        let base = touch_typing_frame();
        for (index, position) in expected_positions.into_iter().enumerate() {
            let frame = workspace_frame(Some(index as i32));
            for row in 0..ROWS {
                for column in 0..COLS {
                    let expected = if (row, column) == position {
                        [0, 255, 0]
                    } else {
                        base[row][column]
                    };
                    assert_eq!(
                        frame[row][column],
                        expected,
                        "workspace {}, cell ({row}, {column})",
                        index + 1
                    );
                }
            }
        }
    }

    #[test]
    fn switching_moves_the_green_indicator_and_missing_targets_leave_macros_white() {
        let third = workspace_frame(Some(2));
        let fourth = workspace_frame(Some(3));
        assert_eq!(third[3][1], [0, 255, 0]);
        assert_eq!(third[2][1], WHITE);
        assert_eq!(fourth[3][1], WHITE);
        assert_eq!(fourth[2][1], [0, 255, 0]);
        for index in [None, Some(-1), Some(5), Some(9), Some(i32::MAX)] {
            assert_eq!(workspace_frame(index), touch_typing_frame());
        }
    }

    #[test]
    fn every_cell_matches_the_finger_zone_specification() {
        let rows = [
            "WWXWWWWWWWWWWWWWWWWWWWW",
            "WWRROYGGCCBPMMMWWWWWWWW",
            "WWWROYGGCCBPMMMMWWWWWWW",
            "WWWROYGGCCBPMMWEWWWWWWW",
            "WWWWROYGGCCBPMWWWWWWWWW",
            "WWWDWWWWWWWWWWWWWWWWWWW",
            "WWWWWWWWWWWWWWWWWWWWWWW",
            "WWWWWWWWWWWWWWWWWWWWWWW",
        ];
        let frame = touch_typing_frame();
        for (r, spec) in rows.iter().enumerate() {
            let spec: Vec<_> = spec.chars().filter(|c| *c != ' ').collect();
            assert_eq!(spec.len(), COLS, "row {r}");
            for (c, symbol) in spec.into_iter().enumerate() {
                let expected = match symbol {
                    'W' => WHITE,
                    'R' => RED,
                    'O' => ORANGE,
                    'Y' => YELLOW,
                    'G' => GREEN,
                    'C' => CYAN,
                    'B' => BLUE,
                    'P' => PURPLE,
                    'M' => PINK,
                    'D' => [0xA8, 0, 0x30],
                    'E' => [0, 255, 0],
                    'X' => [255, 0, 0],
                    _ => panic!("Unknown color"),
                };
                assert_eq!(frame[r][c], expected, "cell ({r}, {c})");
            }
        }
    }

    #[test]
    fn driver_payload_contains_all_row_headers_and_rgb_values() {
        let mut frame = [[WHITE; COLS]; ROWS];
        frame[3][15] = [1, 2, 3];
        let bytes = encode_frame(&frame);
        assert_eq!(bytes.len(), 576);
        for row in 0..ROWS {
            assert_eq!(&bytes[row * 72..row * 72 + 3], &[row as u8, 0, 22]);
        }
        assert_eq!(&bytes[3 * 72 + 3 + 15 * 3..3 * 72 + 3 + 16 * 3], &[1, 2, 3]);
        assert_eq!(&bytes[573..], &[255, 255, 255]);
    }
}
