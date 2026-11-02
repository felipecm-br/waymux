//! The effort control of the `launch` and `switch` menus: `Ctrl+H` / `Ctrl+L` (or `←` / `→`)
//! step the effort of the agent under the cursor, shown as a gauge in the footer, and the pick
//! starts the agent with it (`--effort <level>`).
//!
//! Waymaker runs `waymux launch --effort <step> {}` from the preset's binds (`Transform`), with
//! the cursor row's text; this prints `SetStyledFooter(…)`. When the menu opens, waymux saves
//! next to it ([`Sidecar`]) which agent each row starts, the levels it accepts, its default, and
//! the preset's footer, so the gauge lands right after the footer's `Effort` label in the
//! preset's own colors. The chosen level lives in a runtime file until the menu closes; an
//! untouched menu passes no flag, so each agent keeps its own default.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::PathBuf;

/// Effort levels in order, lowest first (Claude Code's `--effort`). Codex has no such flag (it
/// takes `-c model_reasoning_effort=<level>`): its rows carry their levels as start args.
pub const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Cells of the gauge's track: the longest scale a row may carry (`none` … `max`, or Codex's
/// `low` … `ultra`); shorter scales pad to it, so the footer keeps its width.
const TRACK: usize = 6;

/// The levels an agent accepts at start, empty when it takes no effort flag.
pub fn levels_for(agent: &str) -> &'static [&'static str] {
    if agent.contains("claude") {
        &LEVELS
    } else {
        &[]
    }
}

/// The level the agent uses when started without a flag: Claude's per-model setting, else
/// `medium`.
pub fn default_for(agent: &str, args: &str) -> String {
    let found = if agent.contains("claude") {
        let model = args
            .split_whitespace()
            .skip_while(|a| *a != "--model")
            .nth(1)
            .unwrap_or_default();
        claude_default(&crate::switch::claude_settings(), model)
    } else {
        None
    };
    found
        .filter(|l| LEVELS.contains(&l.as_str()))
        .unwrap_or_else(|| "medium".into())
}

fn claude_default(settings: &Value, model: &str) -> Option<String> {
    let per_model = (!model.is_empty())
        .then(|| {
            let prefix = format!("claude-{model}-");
            settings["modelSettings"].as_object().and_then(|m| {
                m.iter()
                    .find(|(k, _)| k.starts_with(&prefix))
                    .and_then(|(_, v)| v["effortLevel"].as_str().map(str::to_string))
            })
        })
        .flatten();
    per_model.or_else(|| settings["effortLevel"].as_str().map(str::to_string))
}

/// One effort of a row whose levels are other start args (a model id per effort).
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct LevelArgs {
    pub level: String,
    pub args: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct SidecarRow {
    /// The row as Waymaker shows it, without color.
    pub plain: String,
    pub agent: String,
    pub default: String,
    /// The row's own levels, lowest first; empty: the agent's [`levels_for`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
}

impl SidecarRow {
    /// A launch row: its own levels when it carries them (the default being the level its args
    /// start at), else the agent's flag levels and default.
    pub fn for_row(plain: String, agent: &str, args: &str, efforts: &[LevelArgs]) -> Self {
        if efforts.is_empty() {
            return Self {
                plain,
                agent: agent.into(),
                default: default_for(agent, args),
                levels: Vec::new(),
            };
        }
        let default = efforts
            .iter()
            .find(|e| e.args.trim() == args.trim())
            .or_else(|| efforts.iter().find(|e| e.level == "medium"))
            .unwrap_or(&efforts[0])
            .level
            .clone();
        Self {
            plain,
            agent: agent.into(),
            default,
            levels: efforts.iter().map(|e| e.level.clone()).collect(),
        }
    }

    fn levels(&self) -> Vec<&str> {
        if self.levels.is_empty() {
            levels_for(&self.agent).to_vec()
        } else {
            self.levels.iter().map(String::as_str).collect()
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct Sidecar {
    /// The preset's `[footer] content`, with a `… Effort}` label where the gauge goes.
    pub footer: String,
    pub rows: Vec<SidecarRow>,
}

fn dir() -> PathBuf {
    crate::presets::runtime_dir()
}

/// `<menu>.rows` and `<menu>.effort` in the runtime dir; `menu` is `launch` or `switch`.
fn sidecar_path(menu: &str) -> PathBuf {
    dir().join(format!("{menu}.rows"))
}

fn state_path(menu: &str) -> PathBuf {
    dir().join(format!("{menu}.effort"))
}

/// Saves the rows and footer of a menu about to open, and forgets the last menu's choice.
pub fn open(menu: &str, sidecar: &Sidecar) {
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::remove_file(state_path(menu));
    if let Ok(json) = serde_json::to_string(sidecar) {
        let _ = std::fs::write(sidecar_path(menu), json);
    }
}

/// The level chosen in the menu that just closed, if the user stepped it.
pub fn chosen(menu: &str) -> Option<String> {
    let level = std::fs::read_to_string(state_path(menu)).ok()?;
    let level = level.trim().to_string();
    (!level.is_empty() && level.len() <= 16 && level.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(level)
}

/// Clears the menu's choice and rows once the pick is handled.
pub fn close(menu: &str) {
    let _ = std::fs::remove_file(state_path(menu));
    let _ = std::fs::remove_file(sidecar_path(menu));
}

/// The `[footer] content` of the last of these presets that has one.
pub fn preset_footer(paths: &[String]) -> String {
    paths
        .iter()
        .rev()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| t.parse::<toml::Table>().ok())
        .find_map(|t| {
            t.get("footer")?
                .get("content")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// `--effort <step> <row text…>`: steps (or with `0` only shows) the effort of the row's agent
/// and prints the footer for Waymaker's `Transform`. Always exits 0: a failure leaves the
/// footer as it was rather than breaking the menu.
pub fn run_step(menu: &str, args: &[String]) -> i32 {
    let step: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let row_text = crate::theme::strip_ansi(&args.get(1..).unwrap_or_default().join(" "));
    let Some(sidecar) = std::fs::read_to_string(sidecar_path(menu))
        .ok()
        .and_then(|s| serde_json::from_str::<Sidecar>(&s).ok())
    else {
        return 0;
    };
    let row = sidecar
        .rows
        .iter()
        .find(|r| r.plain.trim() == row_text.trim());
    let (levels, current) = match row {
        Some(r) => {
            let levels = r.levels();
            let current = chosen(menu)
                .filter(|l| levels.contains(&l.as_str()))
                .unwrap_or_else(|| r.default.clone());
            (levels, current)
        }
        None => (Vec::new(), String::new()),
    };
    let current = if step != 0 && !levels.is_empty() {
        let i = levels.iter().position(|l| *l == current).unwrap_or(1);
        let i = if step < 0 {
            i.saturating_sub(1)
        } else {
            (i + 1).min(levels.len() - 1)
        };
        let next = levels[i].to_string();
        let _ = std::fs::write(state_path(menu), &next);
        next
    } else {
        current
    };
    if let Some(footer) = render_footer(&sidecar.footer, &levels, &current) {
        println!("SetStyledFooter({footer})");
    }
    0
}

/// The footer with the gauge after its `Effort` label: `━` up to the level, `●` on it, `─` above
/// it, then the level's name, padded so the items after it never move. An agent without levels
/// gets the key and the label in the muted color and an empty track: nothing to press.
pub fn render_footer(footer: &str, levels: &[&str], current: &str) -> Option<String> {
    let at = footer.find("Effort}")?;
    let label_start = footer[..at].rfind('{')?;
    let muted = footer[label_start + 1..at].trim_end_matches(':');
    let end = at + "Effort}".len();
    let width = 6; // the longest level name: "medium"
    let gauge = if levels.is_empty() {
        format!(
            " {{{muted}:{}}} {{{muted}:{:<width$}}}",
            "─".repeat(TRACK),
            "none"
        )
    } else {
        let i = levels.iter().position(|l| *l == current).unwrap_or(0);
        let mut track = String::new();
        if i > 0 {
            let _ = write!(track, "{{white:{}}}", "━".repeat(i));
        }
        track.push_str("{white,bold:●}");
        if i + 1 < levels.len() {
            let _ = write!(track, "{{{muted}:{}}}", "─".repeat(levels.len() - i - 1));
        }
        // A shorter scale pads to the longest, so the footer never shifts.
        let pad = " ".repeat(TRACK.saturating_sub(levels.len()));
        format!(" {track}{pad} {{white,bold:{current:<width$}}}")
    };
    let mut out = format!("{}{gauge}{}", &footer[..end], &footer[end..]);
    if levels.is_empty() {
        // The key before the label loses its cyan: there is nothing to step.
        if let Some(key) = out[..label_start].rfind("{cyan,bold:") {
            out.replace_range(key..key + "{cyan,bold:".len(), &format!("{{{muted}:"));
        }
    }
    Some(out)
}

/// The args to start a row with: the chosen level's own args when the row carries levels, else
/// its args plus `--effort <level>` when the agent accepts the chosen one; unchanged when the
/// user did not step the gauge.
pub fn with_effort(menu: &str, agent: &str, args: &str, efforts: &[LevelArgs]) -> String {
    let Some(level) = chosen(menu) else {
        return args.to_string();
    };
    if !efforts.is_empty() {
        return efforts
            .iter()
            .find(|e| e.level == level)
            .map_or_else(|| args.to_string(), |e| e.args.clone());
    }
    if levels_for(agent).contains(&level.as_str()) {
        format!("{} --effort {level}", args.trim())
            .trim()
            .to_string()
    } else {
        args.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FOOTER: &str = " {cyan,bold:Enter} {darkgray:Launch} {darkgray:│} {cyan,bold:C-h/l} {darkgray:Effort} {darkgray:│} {cyan,bold:Esc} {darkgray:Shell}";

    #[test]
    fn test_gauge_sits_after_the_effort_label_in_the_preset_colors() {
        let out = render_footer(FOOTER, &LEVELS, "high").unwrap();
        assert!(out.contains(
            "{darkgray:Effort} {white:━━}{white,bold:●}{darkgray:──}  {white,bold:high  } {darkgray:│} {cyan,bold:Esc}"
        ));
        assert!(out.starts_with(" {cyan,bold:Enter} {darkgray:Launch}"));
    }

    #[test]
    fn test_gauge_ends_have_no_empty_runs() {
        let low = render_footer(FOOTER, &LEVELS, "low").unwrap();
        assert!(
            low.contains("{darkgray:Effort} {white,bold:●}{darkgray:────}  {white,bold:low   }")
        );
        let max = render_footer(FOOTER, &LEVELS, "max").unwrap();
        assert!(max.contains("{darkgray:Effort} {white:━━━━}{white,bold:●}  {white,bold:max   }"));
    }

    #[test]
    fn test_short_scales_keep_the_footer_width() {
        let three = render_footer(FOOTER, &["low", "medium", "high"], "medium").unwrap();
        let five = render_footer(FOOTER, &LEVELS, "medium").unwrap();
        // Visible text: drop `{style:` openers and the closing braces.
        let shown = |s: &str| {
            let (mut n, mut in_tag) = (0, false);
            for c in s.chars() {
                match c {
                    '{' => in_tag = true,
                    ':' if in_tag => in_tag = false,
                    '}' => {}
                    _ if !in_tag => n += 1,
                    _ => {}
                }
            }
            n
        };
        assert_eq!(shown(&three), shown(&five));
        let six = ["low", "medium", "high", "xhigh", "max", "ultra"];
        assert_eq!(
            shown(&five),
            shown(&render_footer(FOOTER, &six, "ultra").unwrap())
        );
        assert_eq!(
            shown(&five),
            shown(&render_footer(FOOTER, &[], "").unwrap())
        );
    }

    #[test]
    fn test_agent_without_levels_mutes_the_key_and_shows_an_empty_track() {
        let out = render_footer(FOOTER, &[], "").unwrap();
        assert!(
            out.contains("{darkgray:C-h/l} {darkgray:Effort} {darkgray:──────} {darkgray:none  }")
        );
        assert!(out.contains("{cyan,bold:Enter}") && out.contains("{cyan,bold:Esc}"));
    }

    #[test]
    fn test_hex_muted_colors_of_the_builtin_preset_carry_over() {
        let f = " {cyan,bold:C-h/l} {#7f9bb3:Effort} {#5d7a92:│}";
        let out = render_footer(f, &LEVELS, "medium").unwrap();
        assert!(out.contains(
            "{#7f9bb3:Effort} {white:━}{white,bold:●}{#7f9bb3:───}  {white,bold:medium}"
        ));
    }

    #[test]
    fn test_footer_without_effort_label_is_left_alone() {
        assert_eq!(
            render_footer(" {cyan,bold:Enter} {darkgray:Go}", &LEVELS, "low"),
            None
        );
    }

    #[test]
    fn test_claude_default_prefers_the_model_setting() {
        let s = json!({"effortLevel": "high", "modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}});
        assert_eq!(claude_default(&s, "opus").as_deref(), Some("medium"));
        assert_eq!(claude_default(&s, "sonnet").as_deref(), Some("high"));
        assert_eq!(claude_default(&json!({}), "opus"), None);
    }

    #[test]
    fn test_rows_with_their_own_levels_start_at_the_level_of_their_args() {
        let efforts: Vec<LevelArgs> = ["low", "medium", "high"]
            .iter()
            .map(|l| LevelArgs {
                level: (*l).into(),
                args: format!("--model gemini-4-argon-{l}"),
            })
            .collect();
        let row = SidecarRow::for_row("x".into(), "agy", "--model gemini-4-argon-high", &efforts);
        assert_eq!(row.default, "high");
        assert_eq!(row.levels(), ["low", "medium", "high"]);
        let flag = SidecarRow::for_row("y".into(), "agy", "--model m", &[]);
        assert!(
            flag.levels().is_empty(),
            "agy rows without levels keep no gauge"
        );
    }

    #[test]
    fn test_levels_only_for_agents_that_take_the_flag() {
        for a in ["claude", "jail-claude", "jail-claude-ask"] {
            assert_eq!(levels_for(a).len(), 5, "{a}");
        }
        for a in ["agy", "codex", "devin", "opencode", "shell"] {
            assert!(levels_for(a).is_empty(), "{a}");
        }
    }
}
