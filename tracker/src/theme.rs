use ratatui::style::Color;
use serde::Deserialize;
use std::env;

// ── TOML config schema ────────────────────────────────────────────────────────

/// Raw TOML-deserializable theme config — all fields are optional hex strings.
#[derive(Deserialize, Default)]
struct ThemeConfig {
    cursor_bg: Option<String>,
    cursor_fg: Option<String>,
    step_note: Option<String>,
    step_instrument: Option<String>,
    step_fx_cmd: Option<String>,
    step_fx_val: Option<String>,
    step_empty: Option<String>,
    active_track: Option<String>,
    inactive_track: Option<String>,
    status_bar_bg: Option<String>,
    status_bar_fg: Option<String>,
    screen_title: Option<String>,
}

// ── Resolved theme ────────────────────────────────────────────────────────────

/// Resolved ratatui colors for all named theme keys.
/// All fields default to `Color::Reset` (terminal defaults) when no theme is loaded.
pub struct Theme {
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub step_note: Color,
    pub step_instrument: Color,
    pub step_fx_cmd: Color,
    pub step_fx_val: Color,
    pub step_empty: Color,
    pub active_track: Color,
    pub inactive_track: Color,
    pub status_bar_bg: Color,
    pub status_bar_fg: Color,
    pub screen_title: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            cursor_bg: Color::Reset,
            cursor_fg: Color::Reset,
            step_note: Color::Reset,
            step_instrument: Color::Reset,
            step_fx_cmd: Color::Reset,
            step_fx_val: Color::Reset,
            step_empty: Color::Reset,
            active_track: Color::Reset,
            inactive_track: Color::Reset,
            status_bar_bg: Color::Reset,
            status_bar_fg: Color::Reset,
            screen_title: Color::Reset,
        }
    }
}

// ── Color parsing ─────────────────────────────────────────────────────────────

/// Returns true if the terminal advertises true-color support.
fn is_truecolor() -> bool {
    matches!(
        env::var("COLORTERM").as_deref(),
        Ok("truecolor") | Ok("24bit")
    )
}

/// Parse a `#RRGGBB` hex string into (r, g, b) bytes.
/// Returns `None` if the string is not a valid hex color.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Map an RGB triple to the nearest ANSI-256 color index using Euclidean
/// distance in RGB space. Searches the 6×6×6 color cube (indices 16–231) and
/// the 24-step grayscale ramp (indices 232–255).
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let ri = r as u32;
    let gi = g as u32;
    let bi = b as u32;

    let mut best_idx = 16u8;
    let mut best_dist = u32::MAX;

    // 6×6×6 color cube: index = 16 + 36*r_step + 6*g_step + b_step
    // Step levels: 0→0, 1→95, 2→135, 3→175, 4→215, 5→255
    const CUBE_LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    for (rc, &rv) in CUBE_LEVELS.iter().enumerate() {
        for (gc, &gv) in CUBE_LEVELS.iter().enumerate() {
            for (bc, &bv) in CUBE_LEVELS.iter().enumerate() {
                let idx = (16 + 36 * rc + 6 * gc + bc) as u8;
                let dr = ri.saturating_sub(rv).max(rv.saturating_sub(ri));
                let dg = gi.saturating_sub(gv).max(gv.saturating_sub(gi));
                let db = bi.saturating_sub(bv).max(bv.saturating_sub(bi));
                let dist = dr * dr + dg * dg + db * db;
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = idx;
                }
            }
        }
    }

    // Grayscale ramp: index = 232 + step, values 8, 18, 28, …, 238
    for step in 0u32..24 {
        let v = 8 + step * 10;
        let idx = (232 + step) as u8;
        let d = ri.saturating_sub(v).max(v.saturating_sub(ri));
        let dist = d * d * 3; // equal contribution from r, g, b (all equal in gray)
        // Use actual distance so we compare apples-to-apples
        let dr = ri.saturating_sub(v).max(v.saturating_sub(ri));
        let dg = gi.saturating_sub(v).max(v.saturating_sub(gi));
        let db = bi.saturating_sub(v).max(v.saturating_sub(bi));
        let _ = dist;
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best_idx = idx;
        }
    }

    best_idx
}

/// Convert a `#RRGGBB` hex string to a ratatui `Color`, respecting the
/// terminal's color capability. Returns `None` with a warning logged if the
/// hex value is invalid.
fn hex_to_color(s: &str, key: &str, truecolor: bool) -> Option<Color> {
    match parse_hex_color(s) {
        Some((r, g, b)) => {
            if truecolor {
                Some(Color::Rgb(r, g, b))
            } else {
                Some(Color::Indexed(rgb_to_ansi256(r, g, b)))
            }
        }
        None => {
            eprintln!(
                "tracker: theme warning: invalid hex color {:?} for key '{}' — using terminal default",
                s, key
            );
            None
        }
    }
}

// ── Public loader ─────────────────────────────────────────────────────────────

/// Load the user theme from `~/.config/tracker/theme.toml`.
///
/// - If the file does not exist, returns the default theme (all `Color::Reset`).
/// - If the file exists but is malformed, prints a warning and returns default.
/// - Invalid individual color values are skipped with a warning.
/// - On non-true-color terminals, RGB values are mapped to the nearest ANSI-256.
pub fn load() -> Theme {
    let path = match home_config_path() {
        Some(p) => p,
        None => return Theme::default(),
    };

    if !path.exists() {
        return Theme::default();
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tracker: theme warning: could not read {:?}: {} — using terminal defaults", path, e);
            return Theme::default();
        }
    };

    let config: ThemeConfig = match toml::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tracker: theme warning: malformed TOML in {:?}: {} — using terminal defaults", path, e);
            return Theme::default();
        }
    };

    let tc = is_truecolor();

    macro_rules! resolve {
        ($field:expr, $key:literal) => {
            $field
                .as_deref()
                .and_then(|s| hex_to_color(s, $key, tc))
                .unwrap_or(Color::Reset)
        };
    }

    Theme {
        cursor_bg: resolve!(config.cursor_bg, "cursor_bg"),
        cursor_fg: resolve!(config.cursor_fg, "cursor_fg"),
        step_note: resolve!(config.step_note, "step_note"),
        step_instrument: resolve!(config.step_instrument, "step_instrument"),
        step_fx_cmd: resolve!(config.step_fx_cmd, "step_fx_cmd"),
        step_fx_val: resolve!(config.step_fx_val, "step_fx_val"),
        step_empty: resolve!(config.step_empty, "step_empty"),
        active_track: resolve!(config.active_track, "active_track"),
        inactive_track: resolve!(config.inactive_track, "inactive_track"),
        status_bar_bg: resolve!(config.status_bar_bg, "status_bar_bg"),
        status_bar_fg: resolve!(config.status_bar_fg, "status_bar_fg"),
        screen_title: resolve!(config.screen_title, "screen_title"),
    }
}

fn home_config_path() -> Option<std::path::PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join(".config").join("tracker").join("theme.toml"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_hex() {
        assert_eq!(parse_hex_color("#FF8C00"), Some((0xFF, 0x8C, 0x00)));
        assert_eq!(parse_hex_color("#000000"), Some((0, 0, 0)));
        assert_eq!(parse_hex_color("#ffffff"), Some((255, 255, 255)));
    }

    #[test]
    fn parse_invalid_hex() {
        assert_eq!(parse_hex_color("FF8C00"), None);   // missing #
        assert_eq!(parse_hex_color("#FF8C0"), None);   // too short
        assert_eq!(parse_hex_color("#GGGGGG"), None);  // invalid digits
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn default_theme_is_all_reset() {
        let t = Theme::default();
        assert_eq!(t.cursor_bg, Color::Reset);
        assert_eq!(t.cursor_fg, Color::Reset);
        assert_eq!(t.step_note, Color::Reset);
        assert_eq!(t.status_bar_bg, Color::Reset);
        assert_eq!(t.screen_title, Color::Reset);
    }

    #[test]
    fn ansi256_black_maps_to_16() {
        // Pure black should map to index 16 (the 0,0,0 corner of the color cube).
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
    }

    #[test]
    fn ansi256_white_maps_near_231() {
        // Pure white (255,255,255) should map to index 231 (the 5,5,5 corner).
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
    }

    #[test]
    fn truecolor_hex_gives_rgb_color() {
        let color = hex_to_color("#FF8C00", "cursor_bg", true);
        assert_eq!(color, Some(Color::Rgb(0xFF, 0x8C, 0x00)));
    }

    #[test]
    fn non_truecolor_hex_gives_indexed_color() {
        let color = hex_to_color("#FF8C00", "cursor_bg", false);
        matches!(color, Some(Color::Indexed(_)));
    }

    #[test]
    fn load_missing_file_returns_default() {
        // HOME that has no theme.toml → default theme
        let t = load_from_str(None);
        assert_eq!(t.cursor_bg, Color::Reset);
    }

    #[test]
    fn load_valid_toml_truecolor() {
        let toml = r##"
            cursor_bg = "#FF8C00"
            cursor_fg = "#000000"
            screen_title = "#00FFFF"
        "##;
        let t = load_from_str(Some(toml));
        assert_eq!(t.cursor_bg, Color::Rgb(0xFF, 0x8C, 0x00));
        assert_eq!(t.cursor_fg, Color::Rgb(0, 0, 0));
        assert_eq!(t.screen_title, Color::Rgb(0, 255, 255));
        // unset keys fall back to Reset
        assert_eq!(t.step_note, Color::Reset);
    }

    #[test]
    fn load_invalid_hex_falls_back_to_reset() {
        let toml = r##"cursor_bg = "not-a-color""##;
        let t = load_from_str(Some(toml));
        assert_eq!(t.cursor_bg, Color::Reset);
    }

    #[test]
    fn load_malformed_toml_returns_default() {
        let toml = "cursor_bg = [this is not valid toml";
        let t = load_from_str(Some(toml));
        assert_eq!(t.cursor_bg, Color::Reset);
    }

    /// Test helper: parse a theme from a TOML string (or None → empty theme).
    /// Always treats the terminal as truecolor for deterministic tests.
    fn load_from_str(toml_src: Option<&str>) -> Theme {
        let raw = match toml_src {
            Some(s) => s,
            None => return Theme::default(),
        };
        let config: ThemeConfig = match toml::from_str(raw) {
            Ok(c) => c,
            Err(_) => return Theme::default(),
        };
        let tc = true; // force truecolor in tests

        macro_rules! resolve {
            ($field:expr, $key:literal) => {
                $field
                    .as_deref()
                    .and_then(|s| hex_to_color(s, $key, tc))
                    .unwrap_or(Color::Reset)
            };
        }

        Theme {
            cursor_bg: resolve!(config.cursor_bg, "cursor_bg"),
            cursor_fg: resolve!(config.cursor_fg, "cursor_fg"),
            step_note: resolve!(config.step_note, "step_note"),
            step_instrument: resolve!(config.step_instrument, "step_instrument"),
            step_fx_cmd: resolve!(config.step_fx_cmd, "step_fx_cmd"),
            step_fx_val: resolve!(config.step_fx_val, "step_fx_val"),
            step_empty: resolve!(config.step_empty, "step_empty"),
            active_track: resolve!(config.active_track, "active_track"),
            inactive_track: resolve!(config.inactive_track, "inactive_track"),
            status_bar_bg: resolve!(config.status_bar_bg, "status_bar_bg"),
            status_bar_fg: resolve!(config.status_bar_fg, "status_bar_fg"),
            screen_title: resolve!(config.screen_title, "screen_title"),
        }
    }
}
