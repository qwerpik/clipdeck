//! Material dynamic color: seed → full light/dark scheme pushed into the Slint `Theme` global.

use material_colors::color::Rgb;
use material_colors::theme::ThemeBuilder;
use slint::{Color, ComponentHandle};

use crate::{AppWindow, Theme};

/// Seed colors offered in Settings (first is the default).
pub const SEEDS: [u32; 8] = [
    0x7C5CFF, 0x0061A4, 0x006E1C, 0xB3261E, 0xF57C00, 0xD81B60, 0x00897B, 0x6D4C41,
];

fn c(rgb: Rgb) -> Color {
    Color::from_rgb_u8(rgb.red, rgb.green, rgb.blue)
}

/// Parse `#RRGGBB`.
pub fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16)
        .ok()
        .filter(|v| *v <= 0xFF_FFFF)
}

/// Apply a seed color and brightness to the window.
pub fn apply(app: &AppWindow, seed: u32, dark: bool) {
    let theme = ThemeBuilder::with_source(Rgb::from_u32(seed)).build();
    let s = if dark {
        &theme.schemes.dark
    } else {
        &theme.schemes.light
    };
    let t = app.global::<Theme>();
    t.set_dark(dark);
    t.set_primary(c(s.primary));
    t.set_on_primary(c(s.on_primary));
    t.set_primary_container(c(s.primary_container));
    t.set_on_primary_container(c(s.on_primary_container));
    t.set_secondary(c(s.secondary));
    t.set_on_secondary(c(s.on_secondary));
    t.set_secondary_container(c(s.secondary_container));
    t.set_on_secondary_container(c(s.on_secondary_container));
    t.set_tertiary(c(s.tertiary));
    t.set_on_tertiary(c(s.on_tertiary));
    t.set_tertiary_container(c(s.tertiary_container));
    t.set_on_tertiary_container(c(s.on_tertiary_container));
    t.set_error(c(s.error));
    t.set_on_error(c(s.on_error));
    t.set_error_container(c(s.error_container));
    t.set_on_error_container(c(s.on_error_container));
    t.set_surface(c(s.surface));
    t.set_surface_dim(c(s.surface_dim));
    t.set_surface_bright(c(s.surface_bright));
    t.set_surface_container_lowest(c(s.surface_container_lowest));
    t.set_surface_container_low(c(s.surface_container_low));
    t.set_surface_container(c(s.surface_container));
    t.set_surface_container_high(c(s.surface_container_high));
    t.set_surface_container_highest(c(s.surface_container_highest));
    t.set_on_surface(c(s.on_surface));
    t.set_on_surface_variant(c(s.on_surface_variant));
    t.set_outline(c(s.outline));
    t.set_outline_variant(c(s.outline_variant));
    t.set_inverse_surface(c(s.inverse_surface));
    t.set_inverse_on_surface(c(s.inverse_on_surface));
    t.set_scrim(c(s.scrim));
    let palette = app.global::<crate::Palette>();
    palette.set_color_scheme(if dark {
        slint::language::ColorScheme::Dark
    } else {
        slint::language::ColorScheme::Light
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex() {
        assert_eq!(super::parse_hex("#7C5CFF"), Some(0x7C5CFF));
        assert_eq!(super::parse_hex("nope"), None);
    }
}
