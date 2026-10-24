//! Semantic agent-state colors shared by the status bar, Waybar and the Cockpit.
//!
//! Urgency has to stay distinguishable on every theme, so state colors are fixed per light/dark
//! mode instead of borrowed from the theme's ANSI slots: monochrome themes such as `lumon` map
//! red, yellow, orange and magenta to near-identical blues. An explicit `color` under
//! `[theme.states.<state>]` still wins.
//!
//! Dark palette: worst-case pairwise CIEDE2000 10.8 across normal, deuteranope and protanope
//! vision (Machado 2009 simulation), ≥ 4.5:1 on every built-in dark Omarchy background.
//! Light palette: worst-case CIEDE2000 18.5 for normal vision and ≥ 4.5:1 on the built-in light
//! backgrounds; question and permission converge under red-green deficiency, where the glyph
//! shape carries the difference. The Cockpit-only `review` green (done, not reviewed) is at
//! least 14 CIEDE2000 from every other state for normal vision; under red-green deficiency it
//! nears the yellows and blues, so its glyph (`󰷋`) carries the difference there too.

use crate::config::ThemeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// Collapses state aliases onto the six semantic states.
pub fn canonical_state(state: &str) -> &str {
    match state {
        "awaiting_input" => "question",
        "working" => "busy",
        "hanging" => "stalled",
        other => other,
    }
}

pub fn builtin_color(state: &str, mode: Mode) -> &'static str {
    match (mode, canonical_state(state)) {
        (Mode::Dark, "permission") => "#ff9e57",
        (Mode::Dark, "question") => "#f9e2af",
        (Mode::Dark, "stalled") => "#f5c2e7",
        (Mode::Dark, "error") => "#ff6b7a",
        (Mode::Dark, "busy") => "#89b4fa",
        (Mode::Dark, "review") => "#7ee0b5",
        (Mode::Dark, _) => "#8aa4b8",
        (Mode::Light, "permission") => "#c2410c",
        (Mode::Light, "question") => "#7c6a00",
        (Mode::Light, "stalled") => "#a21caf",
        (Mode::Light, "error") => "#be123c",
        (Mode::Light, "busy") => "#2563eb",
        (Mode::Light, "review") => "#166534",
        (Mode::Light, _) => "#4f6b5f",
    }
}

/// The configured `[theme.states.<state>].color` when set, else the built-in semantic color.
pub fn state_color(theme: Option<&ThemeConfig>, state: &str, mode: Mode) -> String {
    let canonical = canonical_state(state);
    theme
        .and_then(|t| t.states.get(state).or_else(|| t.states.get(canonical)))
        .map(|s| s.color.trim())
        .filter(|c| !c.is_empty())
        .map_or_else(
            || builtin_color(canonical, mode).to_string(),
            str::to_string,
        )
}

/// Unquoted value of a `key = value` line (inline `#` comments after a quoted value ignored).
fn toml_value(raw: &str) -> &str {
    let raw = raw.trim();
    match raw.chars().next() {
        Some(q @ ('"' | '\'')) => raw[1..].split(q).next().unwrap_or(""),
        _ => raw.split_whitespace().next().unwrap_or(""),
    }
}

/// `mode = "light" | "dark"` from an Omarchy `colors.toml`, else the background luminance.
pub fn mode_from_colors_toml(content: &str) -> Mode {
    let mut background = None;
    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = toml_value(value);
        match key.trim() {
            "mode" if value.eq_ignore_ascii_case("light") => return Mode::Light,
            "mode" if value.eq_ignore_ascii_case("dark") => return Mode::Dark,
            "background" => background = relative_luminance(value),
            _ => {}
        }
    }
    match background {
        // Above ~0.179 black text out-contrasts white text: the background is light.
        Some(l) if l > 0.179 => Mode::Light,
        _ => Mode::Dark,
    }
}

/// Theme `colors.toml` in priority order: `$WAYMUX_COLORS`, `~/.config/waymux/colors.toml`,
/// then the active Omarchy theme. None when no file exists (built-in colors apply).
pub fn theme_colors_path() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("WAYMUX_COLORS")
        && !explicit.is_empty()
    {
        candidates.push(std::path::PathBuf::from(explicit));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::PathBuf::from(home);
        candidates.push(home.join(".config/waymux/colors.toml"));
        candidates.push(home.join(".local/state/omarchy/current/theme/colors.toml"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Mode of the active theme; dark when unknown. The Omarchy theme-set hook restarts the
/// daemon, so reading this once at startup follows theme switches.
pub fn detect_mode() -> Mode {
    theme_colors_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or(Mode::Dark, |content| mode_from_colors_toml(&content))
}

/// WCAG 2.x relative luminance of `#rrggbb`.
pub fn relative_luminance(hex: &str) -> Option<f64> {
    let h = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| -> f64 {
        let c = f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0)) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4))
}

/// WCAG 2.x contrast ratio between two `#rrggbb` colors.
pub fn contrast_ratio(a: &str, b: &str) -> Option<f64> {
    let (la, lb) = (relative_luminance(a)?, relative_luminance(b)?);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// WCAG minimum for text (1.4.3) and for UI lines such as borders and separators (1.4.11).
pub const MIN_TEXT_CONTRAST: f64 = 4.5;
pub const MIN_LINE_CONTRAST: f64 = 3.0;

/// An sRGB channel (0-255 scale) as a byte; clamped first, so the cast cannot wrap.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn channel(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// `color` unchanged when it reaches `min_ratio` against `background`, otherwise the smallest
/// step of a blend toward `toward` (usually the theme foreground) that does. Keeps the theme's
/// hue where possible; e.g. lumon's `muted` (1.68:1) becomes a legible slate.
pub fn ensure_contrast(color: &str, background: &str, toward: &str, min_ratio: f64) -> String {
    let parse = |hex: &str| -> Option<[f64; 3]> {
        let h = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let ch = |i: usize| f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0));
        Some([ch(0), ch(2), ch(4)])
    };
    let (Some(from), Some(to)) = (parse(color), parse(toward)) else {
        return color.to_string();
    };
    for step in 0..=20 {
        let t = f64::from(step) / 20.0;
        let mixed = format!(
            "#{:02x}{:02x}{:02x}",
            channel(from[0] + (to[0] - from[0]) * t),
            channel(from[1] + (to[1] - from[1]) * t),
            channel(from[2] + (to[2] - from[2]) * t)
        );
        if contrast_ratio(&mixed, background).is_some_and(|r| r >= min_ratio) {
            return mixed;
        }
    }
    toward.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentStateTheme;
    use std::collections::HashMap;

    const STATES: [&str; 7] = [
        "idle",
        "busy",
        "question",
        "permission",
        "stalled",
        "error",
        "review",
    ];
    // Backgrounds of the built-in Omarchy themes, by mode.
    const DARK_BACKGROUNDS: [&str; 17] = [
        "#1e1e2e", "#060B1E", "#2d353b", "#282828", "#0B0C16", "#1f1f28", "#0c0b0c", "#16242d",
        "#121212", "#222222", "#2e3440", "#111c18", "#05182e", "#2c2525", "#101315", "#1a1b26",
        "#000000",
    ];
    const LIGHT_BACKGROUNDS: [&str; 5] = ["#eff1f5", "#FFFCF0", "#fafafa", "#faf4ed", "#ffffff"];
    /// Regression bar for the built-in palettes (actual minimums: 28 dark, 33 light).
    const MIN_STATE_DELTA_E76: f64 = 20.0;

    // r, g, b, x, y, z, L, a: the CIE formulas' own names.
    #[allow(clippy::many_single_char_names)]
    fn to_lab(hex: &str) -> (f64, f64, f64) {
        let h = hex.trim_start_matches('#');
        let lin = |i: usize| {
            let c = f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap()) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(0), lin(2), lin(4));
        let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.95047;
        let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
        let z = (0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b) / 1.08883;
        let f = |t: f64| {
            if t > 216.0 / 24389.0 {
                t.cbrt()
            } else {
                (24389.0 / 27.0 * t + 16.0) / 116.0
            }
        };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    }

    fn delta_e76(a: &str, b: &str) -> f64 {
        let (l1, a1, b1) = to_lab(a);
        let (l2, a2, b2) = to_lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    #[test]
    fn test_builtin_palettes_meet_text_contrast_on_theme_backgrounds() {
        for (mode, backgrounds) in [
            (Mode::Dark, &DARK_BACKGROUNDS[..]),
            (Mode::Light, &LIGHT_BACKGROUNDS[..]),
        ] {
            for state in STATES {
                for bg in backgrounds {
                    let ratio = contrast_ratio(builtin_color(state, mode), bg).unwrap();
                    assert!(ratio >= 4.5, "{mode:?} {state} on {bg}: {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn test_builtin_states_are_distinct() {
        for mode in [Mode::Dark, Mode::Light] {
            for (i, a) in STATES.iter().enumerate() {
                for b in &STATES[i + 1..] {
                    let d = delta_e76(builtin_color(a, mode), builtin_color(b, mode));
                    assert!(d >= MIN_STATE_DELTA_E76, "{mode:?} {a} vs {b}: ΔE76 {d:.1}");
                }
            }
        }
    }

    #[test]
    fn test_lumon_theme_slots_fail_the_bar() {
        // The theme slots the status bar used under lumon: working vs permission.
        assert!(delta_e76("#9dcae5", "#8bc9eb") < MIN_STATE_DELTA_E76);
    }

    #[test]
    fn test_state_color_prefers_explicit_config() {
        let mut states = HashMap::new();
        states.insert(
            "permission".to_string(),
            AgentStateTheme {
                icon: "󱅭".to_string(),
                color: "#123456".to_string(),
            },
        );
        states.insert(
            "question".to_string(),
            AgentStateTheme {
                icon: "󱜻".to_string(),
                color: String::new(),
            },
        );
        let theme = ThemeConfig {
            active_spinner: "dot".to_string(),
            states,
        };
        assert_eq!(
            state_color(Some(&theme), "permission", Mode::Dark),
            "#123456"
        );
        // Empty color falls back; aliases resolve to their semantic state.
        assert_eq!(state_color(Some(&theme), "question", Mode::Dark), "#f9e2af");
        assert_eq!(
            state_color(Some(&theme), "awaiting_input", Mode::Light),
            "#7c6a00"
        );
        assert_eq!(state_color(None, "working", Mode::Dark), "#89b4fa");
    }

    #[test]
    fn test_mode_from_colors_toml() {
        assert_eq!(mode_from_colors_toml("mode = \"light\"\n"), Mode::Light);
        assert_eq!(
            mode_from_colors_toml("mode = 'dark' # inline\nbackground = \"#ffffff\""),
            Mode::Dark
        );
        // No mode key: decide by background luminance.
        assert_eq!(
            mode_from_colors_toml("background = \"#dfdfdf\""),
            Mode::Light
        );
        assert_eq!(
            mode_from_colors_toml("background = \"#16242d\""),
            Mode::Dark
        );
        assert_eq!(mode_from_colors_toml("garbage"), Mode::Dark);
    }

    #[test]
    fn test_ensure_contrast_lifts_only_what_falls_short() {
        // lumon: muted #304860 on #16242d is 1.68:1.
        let lifted = ensure_contrast("#304860", "#16242d", "#d6e2ee", MIN_TEXT_CONTRAST);
        assert!(contrast_ratio(&lifted, "#16242d").unwrap() >= MIN_TEXT_CONTRAST);
        assert!(
            contrast_ratio(&lifted, "#16242d").unwrap() < 7.0,
            "stays subdued: {lifted}"
        );
        // Already legible colors are left alone.
        assert_eq!(
            ensure_contrast("#8aa4b8", "#16242d", "#d6e2ee", MIN_TEXT_CONTRAST),
            "#8aa4b8"
        );
        // Unparseable input passes through.
        assert_eq!(
            ensure_contrast("Darkgray", "#000000", "#ffffff", 3.0),
            "Darkgray"
        );
    }

    #[test]
    fn test_contrast_ratio_reference_values() {
        assert!((contrast_ratio("#000000", "#ffffff").unwrap() - 21.0).abs() < 1e-9);
        assert!((contrast_ratio("#304860", "#16242d").unwrap() - 1.68).abs() < 0.01);
        assert!(contrast_ratio("nope", "#ffffff").is_none());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Theme files are user-edited: any string is a color or falls back.
            #[test]
            fn color_parsers_never_panic(a in "\\PC{0,9}", b in "#?[0-9a-fA-F]{6}") {
                let _ = relative_luminance(&a);
                let _ = ensure_contrast(&a, &b, &b, 4.5);
                let fixed = ensure_contrast(&b, "#000000", "#ffffff", 4.5);
                prop_assert!(relative_luminance(&fixed).is_some(), "{}", fixed);
                prop_assert!(crate::theme::hex_to_ansi_fg(&a).starts_with("\x1b[38;2;"));
            }
        }
    }
}
