//! `waymux launch`: start an agent CLI in this pane, chosen from a menu of agent × model rows
//! with their quota (`waymux quota`), or the recommended row directly (`--auto`).
//!
//! Rows: Claude Code's models (`[launch] claude_models`), the `launch` rows a quota provider
//! adds to its agents (`[{args, model, tier, quota, level, rec}]`, `rec` marking the model to
//! use when that agent is recommended), and one row per installed `[launch] extra` CLI. A
//! `rows_filter` command may rewrite them (JSON array in, JSON array out), and an `exec` hook
//! starts the pick (`<exec> <agent> [args...]`), e.g. to run it in a sandbox. The pane option
//! `@ai_launch` records how the agent started, so `cockpit restore` can start it again.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::{Config, LaunchConfig};
use crate::quota::Snapshot;
use crate::theme::RESET;

const USAGE: &str = "\
usage: waymux launch [--peek] [--pick] [--auto | --menu | shell | <agent> [args...]]
       waymux launch --next <choice> [args...]

  (none)            The default choice: an armed one (--next, for 30 s), else $WAYMUX_LAUNCH_DEFAULT
                    or [launch] default, else the menu (--auto without a terminal)
  --auto            Start the recommended agent and model
  --menu            Menu of agent × model rows with their quota, cursor on the recommended row
  <agent> [args]    Start that agent (through the [launch] exec hook when set)
  shell             A plain shell
  --peek            Print what would start (\"<agent> [args]\", \"shell\" or \"menu\") instead
  --pick            Show the menu and print the pick instead of starting it
  --next <choice>   Arm a one-shot choice for the next launch (a picker that creates the pane)
  --popup [dir]     The menu in a tmux popup; the pick starts in a new window rooted at dir

Environment: WAYMUX_LAUNCH_DEFAULT, WAYMUX_LAUNCH_EXEC, WAYMUX_LAUNCH_FILTER,
WAYMUX_LAUNCH_NEXT (armed-choice file), WAYMUX_LAUNCH_PRESET, WAYMUX_LAUNCH_PICK_PRESET,
plus the WAYMUX_QUOTA_* settings of `waymux quota`.";

/// How long an armed choice stays valid: long enough for the picker to create the pane.
const NEXT_TTL_SECS: u64 = 30;
const CLAUDE_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
const EXTRA: [&str; 2] = ["devin", "codex"];
const POPUP_BADGE: &str = " 󰚩 ";
/// Settings a wrapper may set; a popup and a new window start with the tmux server's
/// environment, so they are passed with `-e`.
const FORWARDED_ENV: [&str; 10] = [
    "WAYMUX_LAUNCH_EXEC",
    "WAYMUX_LAUNCH_FILTER",
    "WAYMUX_LAUNCH_NEXT",
    "WAYMUX_LAUNCH_PRESET",
    "WAYMUX_LAUNCH_PICK_PRESET",
    "WAYMUX_QUOTA_PROVIDERS",
    "WAYMUX_QUOTA_PREFERENCE",
    "WAYMUX_QUOTA_SESSION_RATE",
    "WAYMUX_QUOTA_STATE_DIR",
    "CLAUDE_CONFIG_DIR",
];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub agent: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub cli: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub quota: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub rec: bool,
}

impl Row {
    #[cfg(test)]
    fn command(&self) -> String {
        if self.args.trim().is_empty() {
            self.agent.clone()
        } else {
            format!("{} {}", self.agent, self.args.trim())
        }
    }

    fn columns(&self) -> (String, String, String, String) {
        let gutter = if self.rec { "⠛" } else { " " };
        (
            gutter.to_string(),
            format!("{} {}", self.icon, pad(&self.cli, 14)),
            pad(&self.model, 20),
            pad(&self.tier, 5),
        )
    }

    /// The row as the menu matches it back (no color).
    fn plain(&self) -> String {
        let (g, cli, model, tier) = self.columns();
        format!("{g}{cli}{model}{tier}{}", self.quota)
    }

    fn colored(&self) -> String {
        let (g, cli, model, tier) = self.columns();
        if self.agent == "shell" {
            // Not an agent: the whole row stays muted.
            return format!(
                "{g}\x1b[90m{cli}{RESET}{model}\x1b[90m{tier}{}{RESET}",
                self.quota
            );
        }
        format!(
            "\x1b[97m{g}\x1b[34m{cli}{RESET}{model}\x1b[90m{tier}{RESET}{}{}{RESET}",
            level_ansi(&self.level),
            self.quota
        )
    }
}

/// Pads to `width` characters, always leaving at least one space.
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(len).max(1)))
}

fn level_ansi(level: &str) -> &'static str {
    match level {
        "green" => "\x1b[32m",
        "yellow" => "\x1b[33m",
        "red" => "\x1b[31m",
        _ => "\x1b[90m",
    }
}

fn launch_config() -> LaunchConfig {
    Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .and_then(|c| c.launch)
        .unwrap_or_default()
}

fn env_or(name: &str, fallback: Option<&String>) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| fallback.cloned())
}

fn next_file() -> PathBuf {
    std::env::var_os("WAYMUX_LAUNCH_NEXT")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || crate::presets::runtime_dir().join("launch.next"),
            PathBuf::from,
        )
}

pub fn run_cli(args: &[String]) -> i32 {
    let mut args: Vec<String> = args.to_vec();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return 0;
        }
        Some("--popup") => return run_popup(args.get(1).map(String::as_str)),
        Some("--next") => {
            if args.len() < 2 {
                eprintln!("waymux launch: --next needs a choice");
                return 2;
            }
            let path = next_file();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            return match std::fs::write(&path, format!("{}\n", args[1..].join(" "))) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("waymux launch: {}: {e}", path.display());
                    1
                }
            };
        }
        _ => {}
    }
    let peek = take_flag(&mut args, "--peek");
    let pick_only = take_flag(&mut args, "--pick");
    let cfg = launch_config();

    let (choice, rest) = resolve_choice(&args, pick_only, peek, &cfg);
    let tty = std::io::stdin().is_terminal() && (pick_only || std::io::stdout().is_terminal());
    let choice = if choice == "menu" && !tty {
        "auto".to_string()
    } else {
        choice
    };

    match choice.as_str() {
        "shell" => finish(peek || pick_only, &cfg, "shell", &[]),
        "menu" if peek => {
            println!("menu");
            0
        }
        "auto" | "menu" => run_rows(choice == "menu", peek, pick_only, &cfg),
        other if other.starts_with('-') => {
            eprintln!("waymux launch: unknown option '{other}' (see --help)");
            2
        }
        agent => finish(peek, &cfg, agent, &rest),
    }
}

/// Names of the `WAYMUX_LAUNCH_*` and `WAYMUX_QUOTA_*` settings in this environment.
fn launcher_env() -> Vec<String> {
    std::env::vars()
        .map(|(name, _)| name)
        .filter(|n| n.starts_with("WAYMUX_LAUNCH_") || n.starts_with("WAYMUX_QUOTA_"))
        .collect()
}

fn forwarded_env() -> Vec<String> {
    FORWARDED_ENV
        .iter()
        .filter_map(|n| std::env::var(n).ok().map(|v| format!("{n}={v}")))
        .collect()
}

fn exe() -> String {
    std::env::current_exe().map_or_else(|_| "waymux".into(), |p| p.to_string_lossy().to_string())
}

/// `--popup`: the menu in a popup over the pane; the pick starts in a new window of the session
/// the popup was opened from, rooted at `dir`. Outside tmux it is the menu in this terminal.
fn run_popup(dir: Option<&str>) -> i32 {
    let dir = dir.map_or_else(
        || {
            std::env::current_dir()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default()
        },
        str::to_string,
    );
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let in_popup = std::env::var_os("TMUX_POPUP").is_some_and(|v| !v.is_empty());
    if !in_tmux {
        return run_cli(&["--menu".to_string()]);
    }
    let env = forwarded_env();
    if !in_popup {
        let origin = crate::tmux_cmd::tmux_out(&["display-message", "-p", "#{session_id}"])
            .unwrap_or_default();
        let cmd = format!(
            "TMUX_POPUP=1 TMUX_ORIGIN_SESSION={} {} launch --popup {}",
            crate::sys::shell_quote(origin.trim()),
            crate::sys::shell_quote(&exe()),
            crate::sys::shell_quote(&dir)
        );
        let theme = crate::theme::ThemePalette::load();
        let border = format!("fg={}", theme.magenta);
        let text_col = std::env::var("TMUX_POPUP_TEXT_COLOR")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".into());
        let text = format!("fg={text_col}");
        let mut args = vec![
            "display-popup",
            "-S",
            &border,
            "-s",
            &text,
            "-b",
            "rounded",
            "-T",
            POPUP_BADGE,
            "-d",
            &dir,
        ];
        for pair in &env {
            args.extend(["-e", pair.as_str()]);
        }
        args.extend(["-E", "-w", "75%", "-h", "60%", &cmd]);
        return i32::from(!crate::tmux_cmd::tmux_status(&args));
    }
    let _ = std::env::set_current_dir(&dir);
    // Inside the popup: the pick ("<agent> [args]", empty on Esc) as `launch --pick` prints it.
    let Ok(out) = Command::new(exe())
        .args(["launch", "--pick"])
        .stdout(Stdio::piped())
        .output()
    else {
        return 1;
    };
    let choice = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if choice.is_empty() {
        return 0;
    }
    let words: Vec<String> = std::iter::once(exe())
        .chain(std::iter::once("launch".to_string()))
        .chain(choice.split_whitespace().map(str::to_string))
        .map(|w| crate::sys::shell_quote(&w))
        .collect();
    let command = format!("exec {}", words.join(" "));
    let origin = std::env::var("TMUX_ORIGIN_SESSION").unwrap_or_default();
    let target = format!("{origin}:");
    let mut args = vec!["new-window"];
    if !origin.is_empty() {
        args.extend(["-t", &target]);
    }
    args.extend(["-c", &dir]);
    for pair in &env {
        args.extend(["-e", pair.as_str()]);
    }
    args.push(&command);
    i32::from(!crate::tmux_cmd::tmux_status(&args))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if args.first().map(String::as_str) == Some(flag) {
        args.remove(0);
        true
    } else {
        false
    }
}

/// The explicit choice, else a fresh armed one (consumed unless peeking), else the default.
fn resolve_choice(
    args: &[String],
    pick_only: bool,
    peek: bool,
    cfg: &LaunchConfig,
) -> (String, Vec<String>) {
    if pick_only {
        return ("menu".into(), Vec::new());
    }
    if let Some((first, rest)) = args.split_first() {
        return (normalize(first), rest.to_vec());
    }
    let path = next_file();
    let armed = read_armed(&path);
    if !peek {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(words) = armed
        && let Some((first, rest)) = words.split_first()
    {
        return (normalize(first), rest.to_vec());
    }
    let default =
        env_or("WAYMUX_LAUNCH_DEFAULT", cfg.default.as_ref()).unwrap_or_else(|| "menu".into());
    (normalize(&default), Vec::new())
}

fn normalize(choice: &str) -> String {
    match choice {
        "--auto" => "auto".into(),
        "--menu" => "menu".into(),
        other => other.to_string(),
    }
}

fn read_armed(path: &std::path::Path) -> Option<Vec<String>> {
    let age = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    if age.as_secs() > NEXT_TTL_SECS {
        return None;
    }
    let line = std::fs::read_to_string(path).ok()?;
    let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    (!words.is_empty()).then_some(words)
}

fn installed(name: &str) -> bool {
    crate::sys::find_in_path(name).is_some()
}

fn run_rows(menu: bool, peek: bool, pick_only: bool, cfg: &LaunchConfig) -> i32 {
    if menu {
        eprintln!("\x1b[90mreading quota…{RESET}");
    }
    let snap = Snapshot::collect(false, false);
    let models: Vec<String> = cfg
        .claude_models
        .clone()
        .unwrap_or_else(|| CLAUDE_MODELS.map(String::from).to_vec());
    let extra: Vec<String> = cfg
        .extra
        .clone()
        .unwrap_or_else(|| EXTRA.map(String::from).to_vec());
    let rows = build_rows(&snap, &models, &extra, installed);
    let rows = match env_or("WAYMUX_LAUNCH_FILTER", cfg.rows_filter.as_ref()) {
        Some(cmd) => filter_rows(&cmd, rows),
        None => rows,
    };

    let picked = if menu {
        let mut shown = rows.clone();
        shown.push(Row {
            agent: "shell".into(),
            icon: "󰆍".into(),
            cli: "Shell".into(),
            model: "Terminal".into(),
            tier: "-".into(),
            quota: "no agent".into(),
            ..Row::default()
        });
        match show_menu(&shown, pick_only, cfg) {
            MenuResult::Picked(i) => shown.get(i).cloned(),
            MenuResult::Cancelled => return 0,
            MenuResult::Unmatched => None,
        }
    } else {
        rows.iter().find(|r| r.rec).cloned()
    };

    let Some(row) = picked else {
        // No rows (no quota data, no CLIs) or an unmatched pick: the recommended agent, else the
        // first agent CLI this machine has.
        let order: Vec<String> = std::iter::once(snap.recommend.clone())
            .chain(
                snap.agents
                    .iter()
                    .filter_map(|a| a["agent"].as_str().map(str::to_string)),
            )
            .chain(extra)
            .collect();
        let agent = order
            .iter()
            .find(|a| installed(a))
            .cloned()
            .unwrap_or_else(|| snap.recommend.clone());
        return finish(peek || pick_only, cfg, &agent, &[]);
    };
    if row.agent == "shell" {
        return finish(peek || pick_only, cfg, "shell", &[]);
    }
    if !menu && !peek && !pick_only {
        println!("\x1b[90m{}{RESET}", row.plain());
    }
    let args: Vec<String> = row.args.split_whitespace().map(str::to_string).collect();
    finish(peek || pick_only, cfg, &row.agent, &args)
}

/// Prints the choice (`print_only`), or tags the pane and starts it.
fn finish(print_only: bool, cfg: &LaunchConfig, agent: &str, args: &[String]) -> i32 {
    let line = if args.is_empty() {
        agent.to_string()
    } else {
        format!("{agent} {}", args.join(" "))
    };
    if print_only {
        println!("{line}");
        return 0;
    }
    mark_launch(agent, &line);
    let _ = std::io::stdout().flush();
    let hook = env_or("WAYMUX_LAUNCH_EXEC", cfg.exec.as_ref());
    let mut cmd = if agent == "shell" {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "bash".into());
        Command::new(shell)
    } else if let Some(hook) = &hook {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(format!("{hook} \"$@\""))
            .arg("sh")
            .arg(agent)
            .args(args);
        c
    } else {
        let mut c = Command::new(agent);
        c.args(args);
        c
    };
    // The launcher's own settings stop here: the agent and anything it runs (a test suite, a
    // nested launch) must not inherit another session's quota state or armed choice.
    for name in launcher_env() {
        cmd.env_remove(name);
    }
    let err = cmd.exec();
    eprintln!("waymux launch: {agent}: {err}");
    127
}

/// Records how this pane's agent started (`@ai_launch`); a plain shell clears it.
fn mark_launch(agent: &str, line: &str) {
    let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else {
        return;
    };
    if agent == "shell" {
        let _ =
            crate::tmux_cmd::tmux_status(&["set-option", "-p", "-u", "-t", &pane, "@ai_launch"]);
    } else {
        let _ =
            crate::tmux_cmd::tmux_status(&["set-option", "-p", "-t", &pane, "@ai_launch", line]);
    }
}

fn clock(snap_now: i64, ts: i64) -> String {
    use chrono::TimeZone;
    let fmt = if ts - snap_now > 86_400 {
        "%a %H:%M"
    } else {
        "%H:%M"
    };
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map_or_else(|| "-".into(), |d| d.format(fmt).to_string())
}

fn tier(model: &str) -> &'static str {
    if model.contains("fable") || model.contains("argon") {
        "$$$$"
    } else if model.contains("opus") {
        "$$$"
    } else if model.contains("sonnet") {
        "$$"
    } else {
        "$"
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// The model a Claude session should start on at this quota level.
fn claude_model_for(level: &str) -> &'static str {
    match level {
        "yellow" => "sonnet",
        "red" => "haiku",
        _ => "opus",
    }
}

fn claude_rows(c: &Value, models: &[String], now: i64) -> Vec<Row> {
    #[allow(clippy::cast_possible_truncation)] // percentages
    let pct = |v: &Value| v.as_f64().map_or(0, |x| x.floor() as i64);
    let mut quota = if c["five"].is_null() {
        "quota ?".to_string()
    } else {
        format!("5h {}% · 7d {}%", pct(&c["five"]), pct(&c["seven"]))
    };
    if let Some(until) = c["until"].as_i64() {
        let _ = write!(quota, " ⏳{}", clock(now, until));
    }
    let level = c["level"].as_str().unwrap_or("unknown");
    let want = claude_model_for(level);
    models
        .iter()
        .map(|m| Row {
            agent: "claude".into(),
            args: format!("--model {m}"),
            icon: c["icon"].as_str().unwrap_or_default().to_string(),
            cli: c["title"].as_str().unwrap_or("Claude Code").to_string(),
            model: capitalize(m),
            tier: tier(m).into(),
            quota: quota.clone(),
            level: level.into(),
            rec: m == want,
        })
        .collect()
}

/// The rows a provider attached to its agent (`launch`), with the agent's icon and title.
fn provider_rows(a: &Value) -> Vec<Row> {
    let Some(list) = a["launch"].as_array() else {
        return Vec::new();
    };
    let agent = a["agent"].as_str().unwrap_or_default();
    let text = |r: &Value, key: &str| r[key].as_str().map(str::to_string);
    list.iter()
        .map(|r| {
            let model = text(r, "model").unwrap_or_else(|| "default".into());
            Row {
                agent: agent.to_string(),
                args: text(r, "args").unwrap_or_default(),
                icon: text(r, "icon")
                    .or_else(|| text(a, "icon"))
                    .unwrap_or_default(),
                cli: text(r, "cli")
                    .or_else(|| text(a, "title"))
                    .unwrap_or_else(|| agent.into()),
                tier: text(r, "tier").unwrap_or_else(|| tier(&model.to_lowercase()).into()),
                quota: text(r, "quota").unwrap_or_default(),
                level: text(r, "level").unwrap_or_else(|| "unknown".into()),
                rec: r["rec"] == true,
                model,
            }
        })
        .collect()
}

/// Claude's rows, then each provider agent's, then installed CLIs without telemetry; exactly one
/// row is recommended: the agent `quota` recommends on the model its rows mark, else the first.
pub(crate) fn build_rows(
    snap: &Snapshot,
    claude_models: &[String],
    extra: &[String],
    have: impl Fn(&str) -> bool,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for a in snap.agents.iter().filter(|a| a["available"] == true) {
        if a["agent"] == "claude" {
            rows.extend(claude_rows(a, claude_models, snap.now));
        } else {
            rows.extend(provider_rows(a));
        }
    }
    let icons = crate::icons::Registry::global();
    for name in extra.iter().filter(|n| have(n)) {
        rows.push(Row {
            agent: name.clone(),
            icon: icons.agent(name).to_string(),
            cli: capitalize(name),
            model: "default".into(),
            tier: "-".into(),
            quota: "no quota data".into(),
            level: "unknown".into(),
            ..Row::default()
        });
    }
    let mut found = false;
    for r in &mut rows {
        r.rec = r.rec && !found && r.agent == snap.recommend;
        found |= r.rec;
    }
    if !found && let Some(first) = rows.first_mut() {
        first.rec = true;
    }
    rows
}

/// Rows through the `rows_filter` command; on any failure the rows stay as they were.
fn filter_rows(cmd: &str, rows: Vec<Row>) -> Vec<Row> {
    let Ok(input) = serde_json::to_vec(&rows) else {
        return rows;
    };
    let child = Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return rows;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input);
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            serde_json::from_slice::<Vec<Row>>(&out.stdout).unwrap_or(rows)
        }
        _ => rows,
    }
}

enum MenuResult {
    Picked(usize),
    Unmatched,
    Cancelled,
}

fn wm_bin() -> Option<PathBuf> {
    let local = PathBuf::from(crate::sys::expand_home("~/.local/bin/wm"));
    if local.is_file() {
        return Some(local);
    }
    crate::sys::find_in_path("wm")
}

fn preset_args(pick_only: bool, cfg: &LaunchConfig) -> Vec<String> {
    let Some(preset) = env_or("WAYMUX_LAUNCH_PRESET", cfg.preset.as_ref()) else {
        let (name, esc) = if pick_only {
            ("launch-picker-close", "Close")
        } else {
            ("launch-picker-shell", "Shell")
        };
        let path = crate::presets::materialize_menu_toml(name, "Launch", "Recommended", esc);
        return vec!["-o".to_string(), path.to_string_lossy().to_string()];
    };
    let mut args = vec!["-o".to_string(), crate::sys::expand_home(&preset)];
    if pick_only
        && let Some(overlay) = env_or("WAYMUX_LAUNCH_PICK_PRESET", cfg.pick_preset.as_ref())
    {
        args.extend(["-o".to_string(), crate::sys::expand_home(&overlay)]);
    }
    args
}

/// The Waymaker menu over the rows, cursor on the recommended one. `Esc` cancels.
fn show_menu(rows: &[Row], pick_only: bool, cfg: &LaunchConfig) -> MenuResult {
    let Some(wm) = wm_bin() else {
        eprintln!("waymux launch: Waymaker (`wm`) not found; starting the recommended row");
        return rows
            .iter()
            .position(|r| r.rec)
            .map_or(MenuResult::Unmatched, MenuResult::Picked);
    };
    let pos = rows.iter().position(|r| r.rec).unwrap_or(0);
    let child = Command::new(wm)
        .args(preset_args(pick_only, cfg))
        .args(["--pos", &pos.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return MenuResult::Unmatched;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let input: String = rows.iter().map(|r| r.colored() + "\n").collect();
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return MenuResult::Cancelled;
    };
    if !out.status.success() {
        return MenuResult::Cancelled;
    }
    let sel = crate::theme::strip_ansi(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'));
    rows.iter()
        .position(|r| r.plain() == sel)
        .map_or(MenuResult::Unmatched, MenuResult::Picked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    fn snap(agents: Vec<Value>, rec: &str) -> Snapshot {
        Snapshot {
            now: NOW,
            agents,
            recommend: rec.into(),
            reason: String::new(),
        }
    }

    fn claude(level: &str) -> Value {
        json!({"agent": "claude", "title": "Claude Code", "available": true, "icon": "C",
               "five": 42.5, "seven": 22, "level": level, "until": null})
    }

    fn agy() -> Value {
        json!({"agent": "agy", "title": "Antigravity", "available": true, "icon": "A", "level": "green",
               "launch": [
                 {"args": "--model gemini-4-argon-medium", "model": "Gemini 4 Argon", "quota": "25% left", "level": "green", "rec": true},
                 {"args": "--model gemini-3.8-flash-medium", "model": "Gemini 3.8 Flash", "quota": "97% left", "level": "green"}]})
    }

    fn models() -> Vec<String> {
        CLAUDE_MODELS.map(String::from).to_vec()
    }

    #[test]
    fn test_claude_model_follows_its_quota_level() {
        for (level, want) in [
            ("green", "opus"),
            ("unknown", "opus"),
            ("yellow", "sonnet"),
            ("red", "haiku"),
        ] {
            let rows = build_rows(&snap(vec![claude(level)], "claude"), &models(), &[], |_| {
                false
            });
            let rec: Vec<String> = rows.iter().filter(|r| r.rec).map(Row::command).collect();
            assert_eq!(rec, vec![format!("claude --model {want}")], "{level}");
        }
        let rows = build_rows(
            &snap(vec![claude("green")], "claude"),
            &models(),
            &[],
            |_| false,
        );
        assert_eq!(rows[0].quota, "5h 42% · 7d 22%");
        assert_eq!(rows[0].tier, "$$$");
    }

    #[test]
    fn test_provider_rows_are_recommended_only_for_the_recommended_agent() {
        let both = vec![claude("red"), agy()];
        let rows = build_rows(&snap(both.clone(), "agy"), &models(), &[], |_| false);
        let rec: Vec<String> = rows.iter().filter(|r| r.rec).map(Row::command).collect();
        assert_eq!(rec, vec!["agy --model gemini-4-argon-medium"]);
        assert_eq!(rows[3].cli, "Antigravity");
        assert_eq!(rows[3].tier, "$$$$");
        let rows = build_rows(&snap(both, "claude"), &models(), &[], |_| false);
        assert_eq!(rows.iter().filter(|r| r.rec).count(), 1);
        assert!(rows.iter().find(|r| r.rec).unwrap().args.contains("haiku"));
    }

    #[test]
    fn test_extra_clis_appear_only_when_installed_and_first_row_is_the_fallback() {
        let extra: Vec<String> = vec!["devin".into(), "codex".into()];
        let rows = build_rows(&snap(vec![], "claude"), &models(), &extra, |n| n == "codex");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].command(), "codex");
        assert!(rows[0].rec);
        let mut gone = claude("green");
        gone["available"] = json!(false);
        assert_eq!(
            build_rows(&snap(vec![gone], "claude"), &models(), &[], |_| true),
            Vec::<Row>::new()
        );
    }

    #[test]
    fn test_plain_row_matches_its_colored_form() {
        let rows = build_rows(
            &snap(vec![claude("green"), agy()], "claude"),
            &models(),
            &[],
            |_| false,
        );
        for r in &rows {
            assert_eq!(crate::theme::strip_ansi(&r.colored()), r.plain());
        }
        assert!(
            rows[0].plain().starts_with("⠛C Claude Code   Opus"),
            "{}",
            rows[0].plain()
        );
    }

    #[test]
    fn test_filter_rewrites_rows_and_failures_keep_them() {
        let rows = build_rows(
            &snap(vec![claude("green")], "claude"),
            &models(),
            &[],
            |_| false,
        );
        let renamed = filter_rows(
            r#"sed 's/"agent":"claude"/"agent":"jailed"/g'"#,
            rows.clone(),
        );
        assert!(renamed.iter().all(|r| r.agent == "jailed"));
        assert_eq!(filter_rows("exit 1", rows.clone()), rows);
        assert_eq!(filter_rows("echo nope", rows.clone()), rows);
    }

    #[test]
    fn test_choice_resolution_prefers_explicit_then_default() {
        let cfg = LaunchConfig {
            default: Some("--auto".into()),
            ..LaunchConfig::default()
        };
        let args: Vec<String> = vec!["claude".into(), "--model".into(), "opus".into()];
        assert_eq!(
            resolve_choice(&args, false, true, &cfg),
            (
                "claude".into(),
                vec!["--model".to_string(), "opus".to_string()]
            )
        );
        assert_eq!(resolve_choice(&[], true, false, &cfg).0, "menu");
        assert_eq!(normalize("--menu"), "menu");
        assert_eq!(pad("Codex", 3), "Codex ");
    }
}
