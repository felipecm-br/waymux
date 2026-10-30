//! Text forms of a quota `Snapshot`: JSON, tmux badges, the statusLine line, the plain report
//! and the visual gauges.

use serde_json::{Value, json};
use std::fmt::Write as _;

use super::Snapshot;
use crate::theme::{BOLD, RESET};

const RULE_WIDTH: usize = 74;
/// The recommended-row marker the launch and switch menus share.
const MARK: &str = "\x1b[97m\x1b[1m⠛\x1b[0m";

pub fn json(snap: &Snapshot) -> String {
    json!({
        "now": snap.now,
        "recommend": snap.recommend,
        "reason": snap.reason,
        "agents": snap.agents,
    })
    .to_string()
}

fn level(a: &Value) -> &str {
    a["level"].as_str().unwrap_or("unknown")
}

fn available(a: &Value) -> bool {
    a["available"] == true
}

fn name(a: &Value) -> &str {
    a["agent"].as_str().unwrap_or_default()
}

fn tag(a: &Value) -> String {
    a["tag"]
        .as_str()
        .or_else(|| a["icon"].as_str())
        .unwrap_or_else(|| name(a))
        .to_string()
}

fn title(snap: &Snapshot, agent: &str) -> String {
    snap.agent(agent)
        .and_then(|a| a["title"].as_str())
        .unwrap_or(agent)
        .to_string()
}

fn tmux_color(a: &Value) -> &'static str {
    match level(a) {
        "green" => "green",
        "yellow" => "yellow",
        "red" => "red",
        _ => "colour244",
    }
}

fn ansi(lvl: &str) -> &'static str {
    match lvl {
        "green" => "\x1b[32m",
        "yellow" => "\x1b[33m",
        "red" => "\x1b[31m",
        _ => "\x1b[90m",
    }
}

fn f(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn i(v: &Value) -> Option<i64> {
    #[allow(clippy::cast_possible_truncation)] // timestamps and counts fit an i64
    v.as_i64().or_else(|| v.as_f64().map(|x| x as i64))
}

/// A number as jq prints it (no `.0` on whole values); `None` prints `alt`.
fn n(v: &Value, alt: &str) -> String {
    match f(v) {
        Some(x) => super::num(x).to_string(),
        None => alt.to_string(),
    }
}

fn floor(v: &Value) -> String {
    f(v).map_or_else(|| "0".into(), |x| super::num(x.floor()).to_string())
}

fn clock(ts: Option<i64>, fmt: &str) -> String {
    use chrono::TimeZone;
    ts.and_then(|t| chrono::Local.timestamp_opt(t, 0).single())
        .map_or_else(|| "-".into(), |d| d.format(fmt).to_string())
}

/// Compact duration: `45m`, `2h5m`, `3d4h`.
fn hm(secs: Option<i64>) -> String {
    let Some(s) = secs else {
        return "-".into();
    };
    let m = s.div_euclid(60);
    if m >= 1440 {
        format!("{}d{}h", m / 1440, (m % 1440) / 60)
    } else if m >= 60 {
        format!("{}h{}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

fn hm_f(hours: f64) -> String {
    #[allow(clippy::cast_possible_truncation)] // a few hours in seconds
    hm(Some((hours * 3600.0) as i64))
}

/// Prompt counts per 30 min over the last 5h, oldest first.
pub fn activity_bins(prompts: &[i64], now: i64) -> Vec<usize> {
    (0..10)
        .map(|i| {
            let start = now - (10 - i) * 1800;
            prompts
                .iter()
                .filter(|&&t| t >= start && t < start + 1800)
                .count()
        })
        .collect()
}

// ── badges and the statusLine line ─────────────────────────────────────────

fn short(a: &Value) -> String {
    if level(a) == "red"
        && let Some(until) = i(&a["until"])
    {
        return format!("⏳{}", clock(Some(until), "%H:%M"));
    }
    if name(a) == "claude" {
        let pct = if a["five"].is_null() {
            "?".to_string()
        } else {
            format!("{}·{}%", floor(&a["five"]), floor(&a["seven"]))
        };
        let up = if f(&a["rate"]).unwrap_or(0.0) >= 1.0 {
            " ↗"
        } else {
            ""
        };
        return format!("{pct}{up}");
    }
    if let Some(groups) = a["groups"].as_array() {
        let parts: Vec<String> = groups
            .iter()
            .filter_map(|g| Some(format!("{}{}", g["abbr"].as_str()?, n(&g["pct"], "?"))))
            .collect();
        return format!("{}%", parts.join(" "));
    }
    if a["est_five"].is_null() {
        format!("{}p", n(&a["prompts_5h"], "0"))
    } else {
        format!("~{}%", n(&a["est_five"], "?"))
    }
}

fn sessions_suffix(a: &Value) -> String {
    match i(&a["sessions"]) {
        Some(s) if s > 0 => format!(" ×{s}"),
        _ => String::new(),
    }
}

pub fn badge(snap: &Snapshot) -> String {
    let parts: Vec<String> = snap
        .agents
        .iter()
        .filter(|a| available(a))
        .map(|a| {
            format!(
                "#[fg={}]{}#[default] {}{}",
                tmux_color(a),
                tag(a),
                short(a),
                sessions_suffix(a)
            )
        })
        .collect();
    let rec = snap
        .agent(&snap.recommend)
        .map_or(snap.recommend.clone(), tag);
    format!("{} │ → {rec}", parts.join(" │ "))
}

pub fn badge_min(snap: &Snapshot) -> String {
    snap.agents
        .iter()
        .find(|a| name(a) == snap.recommend && available(a))
        .map_or_else(
            || snap.recommend.clone(),
            |a| format!("#[fg={}]{}#[default]", tmux_color(a), tag(a)),
        )
}

/// Claude's quota as one line for its statusLine; empty before the first sample.
pub fn line(snap: &Snapshot) -> String {
    let Some(c) = snap.agent("claude") else {
        return String::new();
    };
    if c["five"].is_null() {
        return String::new();
    }
    let mut out = format!("{}5h {}%{RESET}", ansi(level(c)), floor(&c["five"]));
    if f(&c["rate"]).unwrap_or(0.0) >= 1.0 {
        let _ = write!(out, " ↗{}%/h", floor(&c["rate"]));
    }
    let _ = write!(
        out,
        " ⟳{} · 7d {}%",
        clock(i(&c["five_reset"]), "%H:%M"),
        floor(&c["seven"])
    );
    if !c["week_pace"].is_null() {
        let _ = write!(out, " ({}× pace)", n(&c["week_pace"], "-"));
    }
    out
}

// ── plain report ───────────────────────────────────────────────────────────

fn notes(a: &Value, info: &str, warn: &str) -> String {
    let mut out = String::new();
    for note in a["notes"].as_array().into_iter().flatten() {
        let Some(text) = note["text"].as_str() else {
            continue;
        };
        let color = if note["level"] == "warn" { warn } else { info };
        let _ = write!(out, "\n    {color}{text}{RESET}");
    }
    out
}

fn visible_groups(a: &Value) -> Vec<&Value> {
    a["groups"]
        .as_array()
        .map(|g| g.iter().filter(|g| g["hidden"] != true).collect())
        .unwrap_or_default()
}

pub fn report(snap: &Snapshot) -> String {
    let now = snap.now;
    let mut out = String::new();
    for a in snap.agents.iter().filter(|a| available(a)) {
        let active = i(&a["active"]).map_or(String::new(), |n| format!(" ({n} active)"));
        let _ = writeln!(
            out,
            "{}● {}{RESET}  [{}]  sessions: {}{active}  prompts 1h/5h: {}/{}",
            ansi(level(a)),
            name(a),
            level(a),
            n(&a["sessions"], "0"),
            n(&a["prompts_1h"], "0"),
            n(&a["prompts_5h"], "0"),
        );
        if name(a) == "claude" {
            let r5 = i(&a["five_reset"]);
            let r7 = i(&a["seven_reset"]);
            let _ = writeln!(
                out,
                "    5h: {}%  reset {} (in {})  burn: {}%/h  +1 session: {}%/h",
                n(&a["five"], "?"),
                clock(r5, "%H:%M"),
                hm(r5.map(|r| r - now)),
                n(&a["rate"], "-"),
                n(&a["rate_with_new"], "-"),
            );
            let _ = writeln!(
                out,
                "    projected at reset: {}%  full in: {}",
                n(&a["projected_five"], "-"),
                f(&a["eta_full_h"]).map_or_else(|| "-".into(), hm_f),
            );
            let _ = write!(
                out,
                "    7d: {}%  reset {}  pace: {}×",
                n(&a["seven"], "?"),
                clock(r7, "%a %H:%M"),
                n(&a["week_pace"], "-"),
            );
            let _ = write!(out, "{}", claude_stale(a, "\x1b[90m(", ")\x1b[0m"));
        } else if a["groups"].is_array() {
            let rows: Vec<String> = visible_groups(a)
                .iter()
                .map(|g| {
                    let detail = g["detail"]
                        .as_str()
                        .map_or(String::new(), |d| format!(" ({d})"));
                    let reset = i(&g["reset"]).map_or_else(
                        || "-".into(),
                        |r| format!("{} (in {})", clock(Some(r), "%a %H:%M"), hm(Some(r - now))),
                    );
                    format!(
                        "    {}{detail}: {}% left  reset {reset}",
                        g["label"].as_str().unwrap_or_default(),
                        n(&g["pct"], "?")
                    )
                })
                .collect();
            out.push_str(&rows.join("\n"));
            out.push_str(&notes(a, "\x1b[90m", "\x1b[33m"));
        } else {
            if a["est_five"].is_null() {
                out.push_str("    est 5h: unknown (no cap hit observed yet)");
            } else {
                let _ = write!(
                    out,
                    "    est 5h: ~{}% of learned capacity ({} prompts)",
                    n(&a["est_five"], "?"),
                    n(&a["capacity_5h"], "?")
                );
            }
            if let Some(until) = i(&a["until"]) {
                let _ = write!(
                    out,
                    "\n    exhausted ({}) until {} (in {})",
                    a["window"].as_str().unwrap_or("?"),
                    clock(Some(until), "%a %H:%M"),
                    hm(Some(until - now))
                );
            }
            out.push_str(&notes(a, "\x1b[90m", "\x1b[33m"));
        }
        out.push_str("\n\n");
    }
    let _ = write!(out, "→ recommend: {}  ({})", snap.recommend, snap.reason);
    out
}

/// Why Claude's numbers may be old: the last statusLine sample, or none yet.
fn claude_stale(a: &Value, open: &str, close: &str) -> String {
    match i(&a["stale_s"]) {
        Some(s) if s > 900 => format!(
            "\n    {open}last statusline sample {} ago{close}",
            hm(Some(s))
        ),
        Some(_) => String::new(),
        None => format!(
            "\n    {open}no samples yet: set Claude Code's statusLine to `waymux quota statusline`{close}"
        ),
    }
}

// ── visual report ──────────────────────────────────────────────────────────

/// ANSI colors for the visual report: the theme's (`colors.toml`) when one is found, else the
/// terminal's own 16 colors.
pub struct Colors {
    pub blue: String,
    pub cyan: String,
    pub green: String,
    pub yellow: String,
    pub red: String,
    pub muted: String,
    pub fg: String,
}

impl Colors {
    pub fn basic() -> Self {
        Self {
            blue: "\x1b[34m".into(),
            cyan: "\x1b[36m".into(),
            green: "\x1b[32m".into(),
            yellow: "\x1b[33m".into(),
            red: "\x1b[31m".into(),
            muted: "\x1b[90m".into(),
            fg: "\x1b[37m".into(),
        }
    }

    pub fn load() -> Self {
        use crate::theme::hex_to_ansi_fg;
        let Some(path) = crate::status_palette::theme_colors_path() else {
            return Self::basic();
        };
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let green = content
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == "green")
            .map_or_else(
                || "#a6e3a1".to_string(),
                |(_, v)| v.trim().trim_matches(['"', '\'']).to_string(),
            );
        let p = crate::theme::ThemePalette::load();
        Self {
            blue: hex_to_ansi_fg(&p.blue),
            cyan: hex_to_ansi_fg(&p.cyan),
            green: hex_to_ansi_fg(&green),
            yellow: hex_to_ansi_fg(&p.yellow),
            red: hex_to_ansi_fg(&p.red),
            muted: hex_to_ansi_fg(&p.muted),
            fg: hex_to_ansi_fg(&p.fg),
        }
    }

    fn level(&self, lvl: &str) -> &str {
        match lvl {
            "green" => &self.green,
            "yellow" => &self.yellow,
            "red" => &self.red,
            _ => &self.muted,
        }
    }
}

fn rpad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(len)))
}

fn lpad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{}{s}", " ".repeat(width.saturating_sub(len)))
}

/// Cells of a 20-cell gauge for a percentage: any use shows one cell, only 100 fills it.
fn cells20(pct: Option<f64>) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1..=19 after the guards
    match pct {
        Some(p) if p >= 100.0 => 20,
        Some(p) if p > 0.0 => ((p / 5.0).round() as usize).clamp(1, 19),
        _ => 0,
    }
}

/// Used % (█), the projection at the reset (▒) and headroom (░).
fn bar_used(c: &Colors, used: Option<f64>, proj: Option<f64>, lvl: &str) -> String {
    let Some(u_pct) = used else {
        return format!("{}{}{RESET}", c.muted, "░".repeat(20));
    };
    let u = cells20(Some(u_pct));
    let p = match proj {
        Some(pr) if pr > u_pct => cells20(Some(pr.min(100.0))).saturating_sub(u).min(20 - u),
        _ => 0,
    };
    let pcol = if proj.unwrap_or(0.0) >= 90.0 {
        &c.red
    } else {
        &c.yellow
    };
    format!(
        "{}{}{pcol}{}{}{}{RESET}",
        c.level(lvl),
        "█".repeat(u),
        "▒".repeat(p),
        c.muted,
        "░".repeat(20 - u - p)
    )
}

/// Weekly used % with a cyan marker (│) where an even pace would be by now.
#[allow(clippy::cast_precision_loss)] // epoch deltas are far below 2^52
fn bar_week(
    c: &Colors,
    now: i64,
    seven: Option<f64>,
    r7: Option<i64>,
    pace: Option<f64>,
) -> String {
    let Some(s) = seven else {
        return format!("{}{}{RESET}", c.muted, "░".repeat(20));
    };
    let u = cells20(Some(s));
    let lvl = if s >= 98.0 {
        "red"
    } else if s >= 85.0 || (pace.unwrap_or(0.0) >= 1.25 && s >= 40.0) {
        "yellow"
    } else {
        "green"
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=19
    let marker =
        r7.map(|r| ((1.0 - (r - now) as f64 / 604_800.0).clamp(0.0, 1.0) * 19.0).round() as usize);
    (0..20)
        .map(|i| {
            if Some(i) == marker {
                format!("{}{BOLD}│{RESET}", c.cyan)
            } else if i < u {
                format!("{}█{RESET}", c.level(lvl))
            } else {
                format!("{}░{RESET}", c.muted)
            }
        })
        .collect()
}

/// Percent left of a model group.
fn bar_left(c: &Colors, pct: Option<f64>) -> String {
    match pct {
        None => format!("{}{}{RESET}", c.muted, "░".repeat(20)),
        Some(p) if p <= 0.0 => format!("{}{}{RESET}", c.red, "░".repeat(20)),
        Some(p) => {
            let lvl = if p < 20.0 { "yellow" } else { "green" };
            let filled = cells20(Some(p));
            format!(
                "{}{}{}{}{RESET}",
                c.level(lvl),
                "█".repeat(filled),
                c.muted,
                "░".repeat(20 - filled)
            )
        }
    }
}

/// Ten 30-minute buckets of prompts (5h), scaled to the busiest one.
fn sparkline(c: &Colors, bins: &[usize]) -> String {
    const GLYPHS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let max = bins.iter().copied().max().unwrap_or(0).max(1);
    let cells: String = bins
        .iter()
        .map(|&b| {
            if b == 0 {
                format!("{}·{RESET}", c.muted)
            } else {
                format!("{}{}{RESET}", c.cyan, GLYPHS[(b * 7 / max).min(7)])
            }
        })
        .collect();
    format!("{cells} {}(10×30m){RESET} ", c.muted)
}

fn bins(a: &Value) -> Vec<usize> {
    let mut b: Vec<usize> = a["activity"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|x| usize::try_from(x.as_u64().unwrap_or(0)).unwrap_or(0))
                .collect()
        })
        .unwrap_or_default();
    b.resize(10, 0);
    b
}

fn activity_row(c: &Colors, a: &Value) -> String {
    format!(
        "    {}{}{RESET}{}  {}{}{RESET}",
        c.muted,
        rpad("5h activity", 15),
        sparkline(c, &bins(a)),
        c.fg,
        lpad(&format!("{}p / 5h", n(&a["prompts_5h"], "0")), 9)
    )
}

pub fn visual(snap: &Snapshot, c: &Colors) -> String {
    let now = snap.now;
    let rule = format!("{}  {}{RESET}", c.muted, "─".repeat(RULE_WIDTH));
    let mut blocks: Vec<String> = Vec::new();
    for a in snap.agents.iter().filter(|a| available(a)) {
        let lvl = level(a);
        let gutter = if name(a) == snap.recommend { MARK } else { " " };
        let icon = a["icon"].as_str().unwrap_or_default();
        let heading = format!("{icon} {}", title(snap, name(a)));
        let active = i(&a["active"]).map_or(String::new(), |n| format!(" ({n} act)"));
        let mut b = format!(
            "  {gutter} {}{BOLD}{}{RESET}{}{BOLD}{}{RESET}{}sessions:{RESET} {}{}{active}{RESET}  {}prompts 1h/5h:{RESET} {}{}/{}{RESET}\n",
            c.blue,
            rpad(&heading, 16),
            c.level(lvl),
            rpad(lvl, 8),
            c.muted,
            c.fg,
            n(&a["sessions"], "0"),
            c.muted,
            c.fg,
            n(&a["prompts_1h"], "0"),
            n(&a["prompts_5h"], "0"),
        );
        if name(a) == "claude" {
            b.push_str(&visual_claude(c, a, now));
        } else if a["groups"].is_array() {
            let rows: Vec<String> = visible_groups(a)
                .iter()
                .map(|g| {
                    let pct = f(&g["pct"]);
                    let glvl = match pct {
                        Some(0.0) => "red",
                        Some(p) if p >= 20.0 => "green",
                        _ => "yellow",
                    };
                    let reset = i(&g["reset"]).map_or_else(
                        || "-".into(),
                        |r| format!("{} ({})", clock(Some(r), "%a %H:%M"), hm(Some(r - now))),
                    );
                    format!(
                        "    {}{}{RESET}{}  {}{BOLD}{}{RESET}  {}{} {reset}{RESET}",
                        c.muted,
                        rpad(g["label"].as_str().unwrap_or_default(), 15),
                        bar_left(c, pct),
                        c.level(glvl),
                        lpad(&format!("{}% left", n(&g["pct"], "?")), 9),
                        c.muted,
                        if pct == Some(0.0) { "⏳" } else { "⟳" },
                    )
                })
                .collect();
            b.push_str(&rows.join("\n"));
            b.push('\n');
            b.push_str(&activity_row(c, a));
            if let Some(dm) = a["default_model"].as_str().filter(|s| !s.is_empty()) {
                let _ = write!(b, "  {}default: {dm}{RESET}", c.muted);
            }
            b.push_str(&visual_notes(c, a));
        } else {
            let est = if a["est_five"].is_null() {
                "? used".to_string()
            } else {
                format!("~{}% used", n(&a["est_five"], "?"))
            };
            let cap = if a["capacity_5h"].is_null() {
                "no cap hit observed yet".to_string()
            } else {
                format!("cap ~{} prompts", n(&a["capacity_5h"], "?"))
            };
            let _ = writeln!(
                b,
                "    {}{}{RESET}{}  {}{BOLD}{}{RESET}  {}{cap}{RESET}",
                c.muted,
                rpad("est 5h cap", 15),
                bar_used(c, f(&a["est_five"]), None, lvl),
                c.level(lvl),
                lpad(&est, 9),
                c.muted,
            );
            b.push_str(&activity_row(c, a));
            if let Some(until) = i(&a["until"]) {
                let _ = write!(
                    b,
                    "  {}⏳ exhausted ({}) until {} ({}){RESET}",
                    c.red,
                    a["window"].as_str().unwrap_or("?"),
                    clock(Some(until), "%a %H:%M"),
                    hm(Some(until - now))
                );
            }
            b.push_str(&visual_notes(c, a));
        }
        blocks.push(b);
    }
    format!(
        "{}  █ actual  ▒ proj  │ pace  ░ headroom{RESET}\n{rule}\n{}\n{rule}\n  {MARK} {}Recommended{RESET} {}{BOLD}{}{RESET}  {}({}){RESET}",
        c.muted,
        blocks.join("\n\n"),
        c.muted,
        c.fg,
        title(snap, &snap.recommend),
        c.muted,
        snap.reason,
    )
}

fn visual_notes(c: &Colors, a: &Value) -> String {
    let mut out = String::new();
    for note in a["notes"].as_array().into_iter().flatten() {
        let Some(text) = note["text"].as_str() else {
            continue;
        };
        if note["level"] == "warn" {
            let _ = write!(out, "\n    {} {text}{RESET}", c.yellow);
        } else {
            let _ = write!(out, "\n    {}󰋼 {text}{RESET}", c.muted);
        }
    }
    out
}

fn visual_claude(c: &Colors, a: &Value, now: i64) -> String {
    let lvl = level(a);
    let r5 = i(&a["five_reset"]);
    let r7 = i(&a["seven_reset"]);
    let used = |v: &Value| {
        lpad(
            &if v.is_null() {
                "?% used".to_string()
            } else {
                format!("{}% used", floor(v))
            },
            9,
        )
    };
    let mut out = format!(
        "    {}{}{RESET}{}  {}{BOLD}{}{RESET}  {}⟳ {}{RESET}",
        c.muted,
        rpad("5h window", 15),
        bar_used(c, f(&a["five"]), f(&a["projected_five"]), lvl),
        c.level(lvl),
        used(&a["five"]),
        c.muted,
        r5.map_or_else(
            || "-".into(),
            |r| format!("{} ({})", clock(Some(r), "%H:%M"), hm(Some(r - now)))
        ),
    );
    if let Some(p) = f(&a["projected_five"]) {
        let col = if p >= 90.0 { &c.red } else { &c.fg };
        let _ = write!(
            out,
            " {}· proj{RESET} {col}{}%{RESET}",
            c.muted,
            floor(&a["projected_five"])
        );
    }
    let _ = write!(
        out,
        "\n    {}{}{RESET}{}  {}{}{RESET}  {}⟳ {}{RESET}",
        c.muted,
        rpad("7d weekly", 15),
        bar_week(c, now, f(&a["seven"]), r7, f(&a["week_pace"])),
        c.fg,
        used(&a["seven"]),
        c.muted,
        clock(r7, "%a %H:%M"),
    );
    if let Some(p) = f(&a["week_pace"]) {
        let col = if p >= 1.25 { &c.yellow } else { &c.cyan };
        let _ = write!(
            out,
            " {}·{RESET} {col}{}× pace{RESET}",
            c.muted,
            n(&a["week_pace"], "-")
        );
    }
    let burn = if a["rate"].is_null() {
        "idle".to_string()
    } else {
        format!("{}%/h", n(&a["rate"], "-"))
    };
    let full = f(&a["eta_full_h"]).map_or(String::new(), |e| format!(" · full {}", hm_f(e)));
    let _ = write!(
        out,
        "\n{}  {}burn:{RESET} {}{burn}{RESET} {}(+1: {}%/h{full}){RESET}",
        activity_row(c, a),
        c.muted,
        c.fg,
        c.muted,
        n(&a["rate_with_new"], "-"),
    );
    out.push_str(&claude_stale(a, &format!("{}󰋼 ", c.muted), RESET));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn snap(agents: Vec<Value>, rec: &str) -> Snapshot {
        Snapshot {
            now: NOW,
            agents,
            recommend: rec.into(),
            reason: "preferred agent has headroom".into(),
        }
    }

    fn claude() -> Value {
        json!({"agent": "claude", "title": "Claude Code", "available": true, "icon": "C",
               "sessions": 2, "active": 1, "five": 42, "seven": 22, "five_reset": NOW + 7200,
               "seven_reset": NOW + 3 * 86400, "rate": 18, "rate_with_new": 28,
               "projected_five": 98, "eta_full_h": 2.1, "week_pace": 0.4,
               "prompts_1h": 3, "prompts_5h": 9, "activity": [0,0,0,0,0,0,1,2,3,3],
               "stale_s": 10, "level": "yellow", "until": null})
    }

    fn agy() -> Value {
        json!({"agent": "agy", "title": "Antigravity", "available": true, "icon": "A",
               "sessions": 0, "prompts_1h": 0, "prompts_5h": 1, "level": "green",
               "groups": [
                 {"key": "claude", "label": "Claude & GPT", "pct": 100, "reset": NOW + 3600, "abbr": "C"},
                 {"key": "flash", "label": "Gemini Flash", "pct": 97, "reset": NOW + 18000, "abbr": "G", "detail": "gemini-flash"},
                 {"key": "argon", "label": "Gemini 4 Argon", "pct": 25, "reset": null, "abbr": "A"},
                 {"key": "pro", "label": "Gemini pro tier", "pct": 97, "hidden": true}],
               "notes": [{"text": "default model is out of quota", "level": "warn"}]})
    }

    #[test]
    fn test_badges_show_quota_per_agent_and_the_recommendation() {
        let s = snap(vec![claude(), agy()], "agy");
        assert_eq!(
            badge(&s),
            "#[fg=yellow]C#[default] 42·22% ↗ ×2 │ #[fg=green]A#[default] C100 G97 A25% │ → A"
        );
        assert_eq!(badge_min(&s), "#[fg=green]A#[default]");
        let mut red = agy();
        red["level"] = json!("red");
        red["until"] = json!(NOW + 60);
        let s = snap(vec![red], "agy");
        assert!(
            badge(&s).contains('⏳') && !badge(&s).contains('%'),
            "{}",
            badge(&s)
        );
    }

    #[test]
    fn test_line_is_empty_before_the_first_sample() {
        let mut c = claude();
        assert!(
            line(&snap(vec![c.clone()], "claude")).starts_with("\x1b[33m5h 42%\x1b[0m ↗18%/h ⟳")
        );
        c["five"] = Value::Null;
        assert_eq!(line(&snap(vec![c], "claude")), "");
    }

    #[test]
    fn test_report_lists_groups_details_and_notes_but_not_hidden_groups() {
        let r = report(&snap(vec![claude(), agy()], "claude"));
        assert!(r.contains("Gemini 4 Argon: 25% left  reset -"), "{r}");
        assert!(r.contains("Gemini Flash (gemini-flash): 97% left"), "{r}");
        assert!(r.contains("default model is out of quota"), "{r}");
        assert!(!r.contains("Gemini pro tier"), "{r}");
        assert!(
            r.contains("    projected at reset: 98%  full in: 2h6m"),
            "{r}"
        );
        assert!(
            r.ends_with("→ recommend: claude  (preferred agent has headroom)"),
            "{r}"
        );
    }

    #[test]
    fn test_visual_marks_the_recommended_agent_once_plus_the_footer() {
        let v = visual(&snap(vec![claude(), agy()], "agy"), &Colors::basic());
        let plain = crate::theme::strip_ansi(&v.replace('\n', "\u{1}")).replace('\u{1}', "\n");
        assert_eq!(plain.matches('⠛').count(), 2, "{plain}");
        assert!(v.contains(MARK));
        for glyph in ["█", "▒", "│", "Recommended"] {
            assert!(plain.contains(glyph), "missing {glyph}: {plain}");
        }
        assert!(plain.contains("Recommended Antigravity"), "{plain}");
        assert!(!plain.contains("Gemini pro tier"), "{plain}");
    }

    #[test]
    fn test_gauges_keep_twenty_cells() {
        let c = Colors::basic();
        for (used, proj) in [
            (None, None),
            (Some(0.0), None),
            (Some(42.0), Some(98.0)),
            (Some(100.0), Some(120.0)),
        ] {
            let cells = crate::theme::strip_ansi(&bar_used(&c, used, proj, "green"))
                .chars()
                .count();
            assert_eq!(cells, 20, "{used:?} {proj:?}");
        }
        let week = crate::theme::strip_ansi(&bar_week(
            &c,
            NOW,
            Some(50.0),
            Some(NOW + 302_400),
            Some(1.0),
        ));
        assert_eq!(week.chars().count(), 20);
        assert_eq!(week.chars().position(|ch| ch == '│'), Some(10));
        assert_eq!(cells20(Some(0.1)), 1);
        assert_eq!(cells20(Some(99.9)), 19);
    }

    #[test]
    fn test_durations_and_activity_bins() {
        assert_eq!(hm(Some(45 * 60)), "45m");
        assert_eq!(hm(Some(2 * 3600 + 5 * 60)), "2h5m");
        assert_eq!(hm(Some(3 * 86400 + 4 * 3600)), "3d4h");
        assert_eq!(hm(None), "-");
        let b = activity_bins(&[NOW - 10, NOW - 20, NOW - 18000, NOW - 18001], NOW);
        assert_eq!(b, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
    }
}
