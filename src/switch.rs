//! `waymux switch`: change the model or effort of the agent running in a tmux pane, in place,
//! by typing the agent's own slash command (`/model`, `/effort`) into it. The window, scrollback
//! and conversation stay.
//!
//! Built in: Claude Code (`/model <alias>` for `[launch] claude_models`, `/effort <level>`,
//! current model and effort read from its status line and settings), Devin (`/model <name>`) and
//! Codex (`/model`, its own picker). Other agents come from `[switch] rows_command`: the pane's
//! process names in `[switch] agents` are detected, and the command runs as
//! `<cmd> <process> <pane>` with the bottom of the pane on stdin, printing
//! `{"agent", "current", "rows": [{id, command, kind: "model"|"effort", name, tier, active}]}`.
//!
//! A half-typed prompt is never submitted with the command: `[switch] draft` (run as
//! `<draft> -t <pane> --ready|take|put`) sets it aside and brings it back after the switch;
//! without it, or when it is not ready, the switch stops (exit 3).

use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::{Config, SwitchConfig};
use crate::theme::{RESET, strip_ansi};
use crate::tmux_cmd::{tmux_out, tmux_status};

const USAGE: &str = "\
usage: waymux switch [-t pane] [choice]
       waymux switch [-t pane] --peek [choice]
       waymux switch [-t pane] --agent
       waymux switch --popup [pane]

  (none)          Menu of the pane agent's models and effort levels, cursor on the current one
  <choice>        Switch directly (a row id: sonnet, high, gemini-3.8-flash-medium, ...)
  --peek          Print the rows (id, command, label), or the command for <choice>, typing nothing
  --agent         Print \"<agent>\\t<current model>\"; exit 1 when no agent runs in the pane
  --popup         The menu in a tmux popup over the pane

Environment: WAYMUX_SWITCH_AGENT (skip pane detection), WAYMUX_SWITCH_ROWS,
WAYMUX_SWITCH_AGENTS, WAYMUX_SWITCH_DRAFT, WAYMUX_SWITCH_PRESET.";

const BUILTIN: [&str; 3] = ["claude", "codex", "devin"];
const CLAUDE_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
const CLAUDE_EFFORTS: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "auto"];
/// Settings a wrapper may set; a popup's process starts with the tmux server's environment.
const FORWARDED_ENV: [&str; 6] = [
    "WAYMUX_SWITCH_ROWS",
    "WAYMUX_SWITCH_AGENTS",
    "WAYMUX_SWITCH_DRAFT",
    "WAYMUX_SWITCH_PRESET",
    "WAYMUX_QUOTA_STATE_DIR",
    "CLAUDE_CONFIG_DIR",
];

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Row {
    pub id: String,
    pub command: String,
    #[serde(default = "model_kind")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub active: bool,
    /// A row drawn as is (Codex's picker): no gutter mark, its own head and text.
    #[serde(skip)]
    pub label: Option<(String, String)>,
}

fn model_kind() -> String {
    "model".into()
}

/// The agent in a pane, its current model, and what it can switch to.
#[derive(Debug, Default)]
pub struct Menu {
    pub agent: String,
    pub current: String,
    pub rows: Vec<Row>,
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(len).max(1)))
}

impl Row {
    fn model(id: &str, name: &str, tier: &str, command: String, active: bool) -> Self {
        Self {
            id: id.into(),
            command,
            kind: "model".into(),
            name: name.into(),
            tier: tier.into(),
            active,
            label: None,
        }
    }

    fn effort(level: &str, command: String, active: bool) -> Self {
        Self {
            id: level.into(),
            command,
            kind: "effort".into(),
            name: level.into(),
            active,
            ..Self::default()
        }
    }

    /// The plain label (menu matching and `--peek`) and its colored form.
    fn labels(&self, icon: &str) -> (String, String) {
        if let Some((head, text)) = &self.label {
            return (
                format!(" {head}{text}"),
                format!(" \x1b[34m{head}{RESET}{text}"),
            );
        }
        let gutter = if self.active { "⠛" } else { " " };
        if self.kind == "effort" {
            let head = format!("󱐋 {}", pad("Effort", 8));
            let name = pad(&self.name, 27);
            (
                format!("{gutter}{head}{name}"),
                format!("\x1b[97m{gutter}\x1b[34m{head}{RESET}{name}"),
            )
        } else {
            let head = format!("{icon} {}", pad("Model", 8));
            let name = pad(&self.name, 22);
            let tier = pad(&self.tier, 5);
            (
                format!("{gutter}{head}{name}{tier}"),
                format!("\x1b[97m{gutter}\x1b[34m{head}{RESET}{name}\x1b[90m{tier}{RESET}"),
            )
        }
    }
}

fn switch_config() -> SwitchConfig {
    Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .and_then(|c| c.switch)
        .unwrap_or_default()
}

fn claude_models() -> Vec<String> {
    Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .and_then(|c| c.launch)
        .and_then(|l| l.claude_models)
        .unwrap_or_else(|| CLAUDE_MODELS.map(String::from).to_vec())
}

fn env_or(name: &str, fallback: Option<&String>) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| fallback.cloned())
}

/// Process names (besides the built-in agents) the rows command handles.
fn extra_agents(cfg: &SwitchConfig) -> Vec<String> {
    match std::env::var("WAYMUX_SWITCH_AGENTS") {
        Ok(list) if !list.trim().is_empty() => {
            list.split_whitespace().map(str::to_string).collect()
        }
        _ => cfg.agents.clone().unwrap_or_default(),
    }
}

/// Message on the tmux status line when there is one, always on stderr.
fn say(msg: &str) {
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        let _ = tmux_status(&["display-message", msg]);
    }
    eprintln!("waymux switch: {msg}");
}

/// The agent process in the pane: its foreground command, else the first agent process of its
/// session (an agent started through a wrapper or a sandbox).
fn pane_process(pane: &str, known: &[String]) -> Option<String> {
    let info = tmux_out(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_current_command} #{pane_pid}",
    ])?;
    let (cmd, pid) = info.trim().split_once(' ')?;
    if known.iter().any(|k| k == cmd) {
        return Some(cmd.to_string());
    }
    let out = Command::new("ps")
        .args(["-s", pid, "-o", "comm="])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|c| known.iter().any(|k| k == c))
        .map(str::to_string)
}

/// The last 8 non-blank lines of the pane, with their colors.
fn screen(pane: &str) -> Vec<String> {
    let raw = tmux_out(&["capture-pane", "-p", "-e", "-t", pane]).unwrap_or_default();
    let lines: Vec<String> = raw
        .lines()
        .filter(|l| !strip_ansi(l).trim().is_empty())
        .map(str::to_string)
        .collect();
    let skip = lines.len().saturating_sub(8);
    lines[skip..].to_vec()
}

fn plain(lines: &[String]) -> Vec<String> {
    lines.iter().map(|l| strip_ansi(l)).collect()
}

/// Claude Code's model from its status line (`Opus 5.5 · ...`), lowercased.
fn claude_current(plain: &[String], models: &[String]) -> String {
    plain
        .iter()
        .rev()
        .find_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?.to_lowercase();
            let version = words.next()?;
            (models.contains(&name) && version.chars().next()?.is_ascii_digit()).then_some(name)
        })
        .unwrap_or_default()
}

/// Claude Code's effort: the badge it shows after a change (`◐ medium · /effort`), else the level
/// it saves per model in settings.json.
fn claude_effort(plain: &[String], current: &str, settings: &Value) -> String {
    let badge = plain.iter().rev().find_map(|l| {
        let i = l.find(" · /eff")?;
        let word = l[..i].split_whitespace().last()?;
        CLAUDE_EFFORTS.contains(&word).then(|| word.to_string())
    });
    if let Some(level) = badge {
        return level;
    }
    if current.is_empty() {
        return String::new();
    }
    let prefix = format!("claude-{current}-");
    settings["modelSettings"]
        .as_object()
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.starts_with(&prefix))
                .and_then(|(_, v)| v["effortLevel"].as_str().map(str::to_string))
        })
        .unwrap_or_default()
}

fn claude_settings() -> Value {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || crate::sys::expand_home("~/.claude"),
            |d| d.to_string_lossy().to_string(),
        );
    std::fs::read_to_string(format!("{dir}/settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn tier(model: &str) -> &'static str {
    if model.contains("fable") || model.contains("argon") {
        "$$$$"
    } else if model.contains("opus") {
        "$$$"
    } else if model.contains("sonnet") || model.contains("codex") || model.contains("fusion") {
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

/// The menu of a built-in agent.
fn builtin_menu(agent: &str, plain: &[String], models: &[String], settings: &Value) -> Menu {
    let mut menu = Menu {
        agent: agent.into(),
        ..Menu::default()
    };
    match agent {
        "claude" => {
            let current = claude_current(plain, models);
            let effort = claude_effort(plain, &current, settings);
            for m in models {
                menu.rows.push(Row::model(
                    m,
                    &capitalize(m),
                    tier(m),
                    format!("/model {m}"),
                    *m == current,
                ));
            }
            for e in CLAUDE_EFFORTS {
                menu.rows
                    .push(Row::effort(e, format!("/effort {e}"), e == effort));
            }
            menu.current = current;
        }
        "devin" => {
            for (id, name) in [
                ("opus", "Claude Opus"),
                ("sonnet", "Claude Sonnet"),
                ("codex", "Codex"),
                ("fusion", "Fusion"),
            ] {
                menu.rows.push(Row::model(
                    id,
                    name,
                    tier(id),
                    format!("/model {id}"),
                    false,
                ));
            }
        }
        "codex" => {
            // Codex takes no /model argument: its own picker sets the model, then the effort.
            let icon = crate::icons::Registry::global().agent("codex");
            menu.rows.push(Row {
                id: "picker".into(),
                command: "/model".into(),
                label: Some((
                    format!("{icon} {}", pad("Model", 8)),
                    pad("Codex model & effort picker", 32),
                )),
                ..Row::model("picker", "", "", String::new(), false)
            });
        }
        _ => {}
    }
    menu
}

/// The rows command's menu for an agent waymux does not know.
fn hook_menu(cmd: &str, process: &str, pane: &str, plain: &[String]) -> Option<Menu> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} \"$@\""))
        .arg("sh")
        .arg(process)
        .arg(pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(plain.join("\n").as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let rows: Vec<Row> = serde_json::from_value(v["rows"].clone()).ok()?;
    Some(Menu {
        agent: v["agent"].as_str().unwrap_or(process).to_string(),
        current: v["current"].as_str().unwrap_or_default().to_string(),
        rows,
    })
}

/// The pane's agent menu, or `None` when no agent runs there.
fn menu_for(pane: &str, cfg: &SwitchConfig) -> Option<(Menu, Vec<String>)> {
    let extra = extra_agents(cfg);
    let hook = env_or("WAYMUX_SWITCH_ROWS", cfg.rows_command.as_ref());
    let forced = std::env::var("WAYMUX_SWITCH_AGENT")
        .ok()
        .filter(|a| !a.is_empty());
    let (process, lines) = if let Some(agent) = forced {
        (agent, Vec::new())
    } else {
        if pane.is_empty() {
            return None;
        }
        let known: Vec<String> = BUILTIN
            .iter()
            .map(|s| (*s).to_string())
            .chain(extra.iter().cloned())
            .collect();
        (pane_process(pane, &known)?, screen(pane))
    };
    let text = plain(&lines);
    let menu = if BUILTIN.contains(&process.as_str()) {
        builtin_menu(&process, &text, &claude_models(), &claude_settings())
    } else {
        hook_menu(hook.as_deref()?, &process, pane, &text)?
    };
    Some((menu, lines))
}

pub fn run_cli(args: &[String]) -> i32 {
    let mut pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let mut mode = "switch";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                pane = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--peek" => {
                mode = "peek";
                i += 1;
            }
            "--agent" => {
                mode = "agent";
                i += 1;
            }
            "--popup" => {
                let target = args.get(i + 1).cloned().unwrap_or(pane);
                return run_popup(&target);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            _ => break,
        }
    }
    let choice = args.get(i).cloned();
    let cfg = switch_config();

    let Some((menu, lines)) = menu_for(&pane, &cfg) else {
        if mode != "agent" {
            say("No AI agent in this pane");
        }
        return 1;
    };
    if mode == "agent" {
        println!("{}\t{}", menu.agent, menu.current);
        return 0;
    }
    let icon = crate::icons::Registry::global()
        .agent(&menu.agent)
        .to_string();

    let pick = match &choice {
        Some(c) => {
            let Some(r) = menu.rows.iter().find(|r| &r.id == c) else {
                say(&format!("'{c}' is not a model or effort of {}", menu.agent));
                return 2;
            };
            r.clone()
        }
        None if mode == "peek" => {
            for r in &menu.rows {
                println!("{}\t{}\t{}", r.id, r.command, r.labels(&icon).0);
            }
            return 0;
        }
        None => match show_menu(&menu, &icon, &cfg) {
            Some(r) => r,
            None => return 0,
        },
    };
    if mode == "peek" {
        println!("{}\t{}", menu.agent, pick.command);
        return 0;
    }
    send(&pane, &menu.agent, &pick.command, &lines, &cfg)
}

fn wm_bin() -> Option<std::path::PathBuf> {
    let local = std::path::PathBuf::from(crate::sys::expand_home("~/.local/bin/wm"));
    if local.is_file() {
        return Some(local);
    }
    crate::sys::find_in_path("wm")
}

/// The Waymaker menu, cursor on the current model. `Esc` cancels.
fn show_menu(menu: &Menu, icon: &str, cfg: &SwitchConfig) -> Option<Row> {
    let Some(wm) = wm_bin() else {
        say("Waymaker (`wm`) not found; pass the choice instead (waymux switch --peek lists them)");
        return None;
    };
    let preset = env_or("WAYMUX_SWITCH_PRESET", cfg.preset.as_ref()).map_or_else(
        || {
            crate::presets::materialize_menu_toml("switch-picker", "Switch", "Current", "Cancel")
                .to_string_lossy()
                .to_string()
        },
        |p| crate::sys::expand_home(&p),
    );
    let pos = menu
        .rows
        .iter()
        .position(|r| r.active && r.kind == "model")
        .unwrap_or(0);
    let labels: Vec<(String, String)> = menu.rows.iter().map(|r| r.labels(icon)).collect();
    let mut child = Command::new(wm)
        .args(["-o", &preset, "--pos", &pos.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let input: String = labels
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sel = strip_ansi(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'));
    labels
        .iter()
        .position(|(plain, _)| *plain == sel)
        .map(|i| menu.rows[i].clone())
}

/// Whether the prompt line holds typed text: something after the marker (`❯`, `›`, `>`) that is
/// not a dim placeholder.
fn has_draft(lines: &[String]) -> bool {
    let Some(line) = lines.iter().rev().find(|l| {
        let t = skip_sgr(l.trim_start());
        t.starts_with('❯') || t.starts_with('›') || t.starts_with('>')
    }) else {
        return false;
    };
    let t = skip_sgr(line.trim_start());
    let after = t
        .trim_start_matches(['❯', '›', '>'])
        .trim_start_matches([' ', '\t', '\u{a0}']);
    let text: String = strip_ansi(after)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if text.is_empty() {
        return false;
    }
    let dim = ["\x1b[2m", "\x1b[90m"];
    // 256-color greys 240..=249 (`\e[38;5;24Nm`)
    let bytes = after.as_bytes();
    let grey = after.starts_with("\x1b[38;5;24")
        && bytes.get(9).is_some_and(u8::is_ascii_digit)
        && bytes.get(10) == Some(&b'm');
    let placeholder = dim.iter().any(|d| after.starts_with(d)) || grey;
    !placeholder
}

/// The text after any leading color codes.
fn skip_sgr(s: &str) -> &str {
    let mut rest = s;
    while let Some(r) = rest.strip_prefix("\x1b[") {
        match r.find('m') {
            Some(i) if r[..i].chars().all(|c| c.is_ascii_digit() || c == ';') => rest = &r[i + 1..],
            _ => break,
        }
    }
    rest
}

fn draft_cmd(draft: &str, pane: &str, action: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("{draft} \"$@\""))
        .args(["sh", "-t", pane, action])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Types the command into the pane and submits it, keeping any draft out of the way.
fn send(pane: &str, agent: &str, command: &str, lines: &[String], cfg: &SwitchConfig) -> i32 {
    let draft_tool = env_or("WAYMUX_SWITCH_DRAFT", cfg.draft.as_ref());
    let mut kept = false;
    if has_draft(lines) {
        match &draft_tool {
            Some(tool) if draft_cmd(tool, pane, "--ready") && draft_cmd(tool, pane, "take") => {
                kept = true;
            }
            _ => {
                say(&format!(
                    "Draft in the {agent} prompt: send or clear it, then switch again"
                ));
                return 3;
            }
        }
    }
    let _ = tmux_status(&["send-keys", "-t", pane, "-l", "--", command]);
    // Let the agent's input settle (slash-command autocomplete) before submitting.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = tmux_status(&["send-keys", "-t", pane, "Enter"]);
    match (kept, agent, &draft_tool) {
        (true, "codex", _) => say(&format!(
            "{agent} → {command} · draft kept until the picker closes"
        )),
        (true, _, Some(tool)) => {
            std::thread::sleep(std::time::Duration::from_millis(300));
            draft_cmd(tool, pane, "put");
            say(&format!("{agent} → {command} · draft kept"));
        }
        _ => say(&format!("{agent} → {command}")),
    }
    0
}

/// `--popup`: the menu over the pane, titled with the agent and its model.
fn run_popup(pane: &str) -> i32 {
    if std::env::var_os("TMUX_POPUP").is_some_and(|v| !v.is_empty()) {
        return run_cli(&["-t".into(), pane.to_string()]);
    }
    let cfg = switch_config();
    let Some((menu, _)) = menu_for(pane, &cfg) else {
        let _ = tmux_status(&["display-message", "No AI agent in this pane"]);
        return 0;
    };
    let icon = crate::icons::Registry::global()
        .agent(&menu.agent)
        .to_string();
    let title = if menu.current.is_empty() {
        format!(" {icon} {} ", menu.agent)
    } else {
        format!(" {icon} {} · {} ", menu.agent, menu.current)
    };
    let exe = std::env::current_exe()
        .map_or_else(|_| "waymux".into(), |p| p.to_string_lossy().to_string());
    let cmd = format!(
        "TMUX_POPUP=1 {} switch -t {}",
        crate::sys::shell_quote(&exe),
        crate::sys::shell_quote(pane)
    );
    let theme = crate::theme::ThemePalette::load();
    let border = format!("fg={}", theme.magenta);
    let text_col = std::env::var("TMUX_POPUP_TEXT_COLOR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into());
    let text = format!("fg={text_col}");
    let env: Vec<String> = FORWARDED_ENV
        .iter()
        .filter_map(|n| std::env::var(n).ok().map(|v| format!("{n}={v}")))
        .collect();
    let mut args = vec![
        "display-popup",
        "-S",
        &border,
        "-s",
        &text,
        "-b",
        "rounded",
        "-T",
        &title,
    ];
    for pair in &env {
        args.extend(["-e", pair.as_str()]);
    }
    args.extend(["-E", "-w", "75%", "-h", "60%", &cmd]);
    i32::from(!tmux_status(&args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn models() -> Vec<String> {
        ["fable", "opus", "sonnet", "haiku"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn test_claude_rows_mark_the_current_model_and_effort() {
        let screen = lines(&["  Opus 5.5 · ctx 1%", "◐ high · /effort", "❯ "]);
        let menu = builtin_menu("claude", &screen, &models(), &Value::Null);
        assert_eq!(menu.current, "opus");
        let cmds: Vec<&str> = menu.rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(
            &cmds[..5],
            [
                "/model fable",
                "/model opus",
                "/model sonnet",
                "/model haiku",
                "/effort low"
            ]
        );
        assert_eq!(cmds.last(), Some(&"/effort auto"));
        let active: Vec<&str> = menu
            .rows
            .iter()
            .filter(|r| r.active)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(active, ["opus", "high"]);
        let (label, colored) = menu.rows[1].labels("C");
        assert!(label.starts_with("⠛C Model"), "{label}");
        assert_eq!(strip_ansi(&colored), label);
    }

    #[test]
    fn test_claude_effort_falls_back_to_settings() {
        let settings = json!({"modelSettings": {"claude-sonnet-5-5": {"effortLevel": "xhigh"}}});
        assert_eq!(
            claude_effort(&lines(&["  Sonnet 5.5"]), "sonnet", &settings),
            "xhigh"
        );
        assert_eq!(claude_effort(&lines(&[]), "", &settings), "");
    }

    #[test]
    fn test_devin_and_codex_rows() {
        let devin: Vec<String> = builtin_menu("devin", &[], &models(), &Value::Null)
            .rows
            .iter()
            .map(|r| r.command.clone())
            .collect();
        assert_eq!(
            devin,
            [
                "/model opus",
                "/model sonnet",
                "/model codex",
                "/model fusion"
            ]
        );
        let codex = builtin_menu("codex", &[], &models(), &Value::Null);
        assert_eq!(codex.rows.len(), 1);
        assert_eq!(codex.rows[0].command, "/model");
        let (label, colored) = codex.rows[0].labels("X");
        assert!(label.contains("Codex model & effort picker") && !label.contains('⠛'));
        assert_eq!(strip_ansi(&colored), label);
    }

    #[test]
    fn test_draft_detection_ignores_placeholders() {
        assert!(has_draft(&lines(&["❯ half typed"])));
        assert!(has_draft(&lines(&["\x1b[1m❯\x1b[0m fix it"])));
        assert!(!has_draft(&lines(&["❯ "])));
        assert!(!has_draft(&lines(&["❯\u{a0}\x1b[2mTry something\x1b[0m"])));
        assert!(!has_draft(&lines(&["› \x1b[90mAsk Codex\x1b[0m"])));
        assert!(!has_draft(&lines(&[
            "> \x1b[38;5;245mType a message\x1b[0m"
        ])));
        assert!(!has_draft(&lines(&["no prompt here"])));
    }

    #[test]
    fn test_hook_menu_reads_the_rows_command() {
        let cmd = r#"printf '{"agent":"agy","current":"Gemini","rows":[{"id":"m","command":"/model m","name":"M","active":true},{"id":"high","command":"/model m-high","kind":"effort"}]}' #"#;
        let menu = hook_menu(cmd, "agy-bin", "%1", &[]).unwrap();
        assert_eq!(
            (menu.agent.as_str(), menu.current.as_str()),
            ("agy", "Gemini")
        );
        assert_eq!(menu.rows[1].kind, "effort");
        assert!(hook_menu("echo nope", "agy", "%1", &[]).is_none());
    }
}
