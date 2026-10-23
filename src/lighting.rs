//! Native BlackWidow V4 Pro matrix. Coordinates are (row, column).

pub const ROWS: usize = 8;
pub const COLS: usize = 23;
pub type Rgb = [u8; 3];
pub type Frame = [[Rgb; COLS]; ROWS];
pub const WHITE: Rgb = [255, 255, 255];
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
    fn every_cell_matches_the_finger_zone_specification() {
        let rows = [
            "WWWWWWWWWWWWWWWWWWWWWWW",
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
