// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! AutoCAD Color Index (ACI) palette and color resolution.

use serde::Serialize;

/// The full 256-entry ACI palette as RGB.
///
/// Index 0 is BYBLOCK and 256 is BYLAYER; neither is a real color, so entry 0
/// is black and callers resolve those two cases before looking anything up.
pub const ACI_TABLE: [[u8; 3]; 256] = build_table();

const fn build_table() -> [[u8; 3]; 256] {
    let mut t = [[0u8; 3]; 256];
    t[1] = [255, 0, 0];
    t[2] = [255, 255, 0];
    t[3] = [0, 255, 0];
    t[4] = [0, 255, 255];
    t[5] = [0, 0, 255];
    t[6] = [255, 0, 255];
    t[7] = [255, 255, 255];
    t[8] = [128, 128, 128];
    t[9] = [192, 192, 192];

    // 10..=249: 24 hues 15 degrees apart, each with 5 brightness levels in a
    // saturated (even index) and a half-saturated (odd index) variant.
    const LEVELS: [u32; 5] = [255, 204, 153, 127, 76];
    let mut i = 10;
    while i < 250 {
        let hue = (i - 10) / 10; // 0..24
        let shade = (i % 10) / 2; // 0..5
        let tint = i % 2 == 1;
        let q = hue_quarters(hue as u32);
        let v = LEVELS[shade];
        let mut c = 0;
        while c < 3 {
            t[i][c] = if tint {
                (v * (4 + q[c]) / 8) as u8
            } else {
                (v * q[c] / 4) as u8
            };
            c += 1;
        }
        i += 1;
    }

    t[250] = [51, 51, 51];
    t[251] = [80, 80, 80];
    t[252] = [105, 105, 105];
    t[253] = [130, 130, 130];
    t[254] = [190, 190, 190];
    t[255] = [255, 255, 255];
    t
}

/// Fully saturated RGB for a hue step (15 degrees each), in quarters (0..=4).
const fn hue_quarters(hue: u32) -> [u32; 3] {
    let sector = hue / 4;
    let f = hue % 4;
    match sector {
        0 => [4, f, 0],
        1 => [4 - f, 4, 0],
        2 => [0, 4, f],
        3 => [0, 4 - f, 4],
        4 => [f, 0, 4],
        _ => [4, 0, 4 - f],
    }
}

/// RGB for an ACI index. Out-of-range values fall back to white (ACI 7).
pub fn aci_to_rgb(index: i32) -> [u8; 3] {
    if (1..=255).contains(&index) {
        ACI_TABLE[index as usize]
    } else {
        ACI_TABLE[7]
    }
}

/// A fully resolved display color.
///
/// `adaptive` marks ACI 7, which is drawn white on a dark background and black
/// on a light one. `rgb` always holds the dark-background value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Color {
    pub rgb: [u8; 3],
    pub adaptive: bool,
}

impl Color {
    pub const FOREGROUND: Color = Color {
        rgb: [255, 255, 255],
        adaptive: true,
    };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color {
            rgb: [r, g, b],
            adaptive: false,
        }
    }

    pub fn from_aci(index: i32) -> Self {
        if index == 7 || !(1..=255).contains(&index) {
            Color::FOREGROUND
        } else {
            Color {
                rgb: ACI_TABLE[index as usize],
                adaptive: false,
            }
        }
    }

    /// From a DXF group-420 value (`0x00RRGGBB`).
    pub fn from_true_color(value: i32) -> Self {
        let v = value as u32;
        Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// The color to draw with on the given background.
    pub fn display_rgb(&self, dark_background: bool) -> [u8; 3] {
        if self.adaptive && !dark_background {
            [0, 0, 0]
        } else {
            self.rgb
        }
    }

    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.rgb[0], self.rgb[1], self.rgb[2])
    }
}

/// How an entity (or layer) specifies its color in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpec {
    ByLayer,
    ByBlock,
    Aci(u8),
    True(i32),
}

impl ColorSpec {
    /// Resolve against the owning layer's color and the color inherited from
    /// the enclosing INSERT (used for BYBLOCK).
    pub fn resolve(self, layer: Color, block: Color) -> Color {
        match self {
            ColorSpec::ByLayer => layer,
            ColorSpec::ByBlock => block,
            ColorSpec::Aci(i) => Color::from_aci(i as i32),
            ColorSpec::True(v) => Color::from_true_color(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_entries() {
        assert_eq!(aci_to_rgb(1), [255, 0, 0]);
        assert_eq!(aci_to_rgb(2), [255, 255, 0]);
        assert_eq!(aci_to_rgb(3), [0, 255, 0]);
        assert_eq!(aci_to_rgb(4), [0, 255, 255]);
        assert_eq!(aci_to_rgb(5), [0, 0, 255]);
        assert_eq!(aci_to_rgb(6), [255, 0, 255]);
        assert_eq!(aci_to_rgb(7), [255, 255, 255]);
        assert_eq!(aci_to_rgb(8), [128, 128, 128]);
        assert_eq!(aci_to_rgb(9), [192, 192, 192]);
    }

    #[test]
    fn hue_ramp_entries() {
        assert_eq!(aci_to_rgb(10), [255, 0, 0]);
        assert_eq!(aci_to_rgb(11), [255, 127, 127]);
        assert_eq!(aci_to_rgb(12), [204, 0, 0]);
        assert_eq!(aci_to_rgb(15), [153, 76, 76]);
        assert_eq!(aci_to_rgb(19), [76, 38, 38]);
        assert_eq!(aci_to_rgb(20), [255, 63, 0]);
        assert_eq!(aci_to_rgb(21), [255, 159, 127]);
        assert_eq!(aci_to_rgb(30), [255, 127, 0]);
        assert_eq!(aci_to_rgb(50), [255, 255, 0]);
        assert_eq!(aci_to_rgb(90), [0, 255, 0]);
        assert_eq!(aci_to_rgb(130), [0, 255, 255]);
        assert_eq!(aci_to_rgb(170), [0, 0, 255]);
        assert_eq!(aci_to_rgb(210), [255, 0, 255]);
        assert_eq!(aci_to_rgb(240), [255, 0, 63]);
        assert_eq!(aci_to_rgb(249), [76, 38, 47]);
    }

    #[test]
    fn greys_and_range() {
        assert_eq!(aci_to_rgb(250), [51, 51, 51]);
        assert_eq!(aci_to_rgb(254), [190, 190, 190]);
        assert_eq!(aci_to_rgb(255), [255, 255, 255]);
        assert_eq!(aci_to_rgb(0), [255, 255, 255]);
        assert_eq!(aci_to_rgb(999), [255, 255, 255]);
        assert_eq!(aci_to_rgb(-5), [255, 255, 255]);
    }

    #[test]
    fn aci7_adapts_to_background() {
        let c = Color::from_aci(7);
        assert!(c.adaptive);
        assert_eq!(c.display_rgb(true), [255, 255, 255]);
        assert_eq!(c.display_rgb(false), [0, 0, 0]);
        assert_eq!(Color::from_aci(1).display_rgb(false), [255, 0, 0]);
    }

    #[test]
    fn resolve_by_layer_block_and_true_color() {
        let layer = Color::rgb(1, 2, 3);
        let block = Color::rgb(4, 5, 6);
        assert_eq!(ColorSpec::ByLayer.resolve(layer, block), layer);
        assert_eq!(ColorSpec::ByBlock.resolve(layer, block), block);
        assert_eq!(
            ColorSpec::Aci(5).resolve(layer, block),
            Color::rgb(0, 0, 255)
        );
        assert_eq!(
            ColorSpec::True(0x12_34_56).resolve(layer, block),
            Color::rgb(0x12, 0x34, 0x56)
        );
    }
}
