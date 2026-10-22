//! `waymux doctor`: checks what the daemon, Cockpit and worktree commands need and
//! says how to fix each gap. Exits 1 when a required check fails.

use crate::rpc_client;
use crate::sys::find_in_path;
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Ok,
    /// Optional feature missing; Waymux still runs.
    Warn,
    /// Waymux cannot work until this is fixed.
    Fail,
}

struct Check {
    level: Level,
    name: &'static str,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            level: Level::Ok,
            name,
            detail: detail.into(),
            fix: None,
        }
    }

    fn problem(level: Level, name: &'static str, detail: &str, fix: &str) -> Self {
        Check {
            level,
            name,
            detail: detail.to_string(),
            fix: Some(fix.to_string()),
        }
    }
}

/// Bundled `awt-*.sh` helpers that `waymux wt` delegates to.
const WT_SCRIPTS: [&str; 7] = [
    "awt-new.sh",
    "awt-delete.sh",
    "awt-merge.sh",
    "awt-rebase.sh",
    "awt-pr.sh",
    "awt-sweep.sh",
    "awt-reap.sh",
];

fn first_line(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

fn tool(name: &'static str, level: Level, purpose: &str, fix: &str) -> Check {
    match find_in_path(name) {
        Some(path) => Check::ok(name, path.display().to_string()),
        None => Check::problem(level, name, &format!("not found ({purpose})"), fix),
    }
}

fn check_daemon() -> Check {
    let port = rpc_client::daemon_port();
    match rpc_client::http_get_json(port, "/health", 1500) {
        Some((200, _)) => Check::ok("daemon", format!("healthy on 127.0.0.1:{port}")),
        Some((code, _)) => Check::problem(
            Level::Fail,
            "daemon",
            &format!("HTTP {code} from 127.0.0.1:{port}"),
            "systemctl --user restart waymux",
        ),
        None => Check::problem(
            Level::Fail,
            "daemon",
            &format!("not reachable on 127.0.0.1:{port}"),
            "systemctl --user enable --now waymux   (or run `waymux daemon` in a spare pane)",
        ),
    }
}

fn check_token() -> Check {
    match rpc_client::read_token() {
        Some(_) => Check::ok("token", "readable"),
        None => Check::problem(
            Level::Fail,
            "token",
            "no session token under $XDG_RUNTIME_DIR/waymux",
            "start the daemon; it writes the token on startup",
        ),
    }
}

fn check_tmux() -> Check {
    match find_in_path("tmux") {
        Some(_) => {
            let version = first_line("tmux", &["-V"]).unwrap_or_else(|| "tmux".into());
            if std::env::var_os("TMUX").is_some() {
                Check::ok("tmux", version)
            } else {
                Check::problem(
                    Level::Warn,
                    "tmux",
                    &format!("{version}, but this shell is not inside tmux"),
                    "run doctor from a tmux pane to check pane-level features",
                )
            }
        }
        None => Check::problem(
            Level::Fail,
            "tmux",
            "not found",
            "install tmux 3.3 or newer from your package manager",
        ),
    }
}

fn check_sound_player() -> Check {
    let player = crate::sys::default_sound_player();
    if find_in_path(&player).is_some() {
        Check::ok("sound", player)
    } else {
        Check::problem(
            Level::Warn,
            "sound",
            "no pw-play, paplay, aplay or afplay",
            "install pipewire or pulseaudio utilities, or set [sound] enabled = false",
        )
    }
}

fn check_wt_scripts() -> Check {
    let missing: Vec<&str> = WT_SCRIPTS
        .iter()
        .copied()
        .filter(|s| crate::wt::find_waymaker_script(s).is_none())
        .collect();
    if missing.is_empty() {
        let dir = crate::wt::find_waymaker_script(WT_SCRIPTS[0])
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default();
        Check::ok("wt scripts", dir)
    } else {
        Check::problem(
            Level::Warn,
            "wt scripts",
            &format!("missing {}", missing.join(", ")),
            "rerun install.sh, which ships them to ~/.local/share/waymux/scripts",
        )
    }
}

fn check_theme() -> Check {
    match crate::status_palette::theme_colors_path() {
        Some(path) => Check::ok("theme", path.display().to_string()),
        None => Check {
            level: Level::Ok,
            name: "theme",
            detail: "built-in colors (no colors.toml found)".into(),
            fix: Some(
                "optional: point $WAYMUX_COLORS or ~/.config/waymux/colors.toml at a theme".into(),
            ),
        },
    }
}

fn check_claude_hooks() -> Check {
    let Ok(home) = std::env::var("HOME") else {
        return Check::problem(Level::Warn, "agent hooks", "$HOME is unset", "set $HOME");
    };
    let settings = format!("{home}/.claude/settings.json");
    match std::fs::read_to_string(&settings) {
        Ok(content) if content.contains("waymux hook") => {
            Check::ok("agent hooks", format!("Claude Code ({settings})"))
        }
        Ok(_) => Check::problem(
            Level::Warn,
            "agent hooks",
            "Claude Code settings have no `waymux hook` entries",
            "merge the output of `waymux init --hooks` into ~/.claude/settings.json",
        ),
        Err(_) => Check::problem(
            Level::Warn,
            "agent hooks",
            "no ~/.claude/settings.json (other agents are not checked)",
            "see `waymux init --hooks` for the hook wiring",
        ),
    }
}

fn run_checks() -> Vec<Check> {
    let mut checks = vec![check_daemon(), check_token(), check_tmux()];
    checks.push(tool(
        "git",
        Level::Fail,
        "worktrees, diffs",
        "install git from your package manager",
    ));
    checks.push(tool(
        "wm",
        Level::Warn,
        "the Cockpit and `wt` pickers",
        "install Waymaker: https://github.com/felipecm-br/waymaker",
    ));
    checks.push(tool(
        "lazygitrs",
        Level::Warn,
        "review notes",
        "install lazygitrs: https://github.com/felipecm-br/lazygitrs",
    ));
    checks.push(tool(
        "delta",
        Level::Warn,
        "side-by-side diffs",
        "install git-delta (falls back to bat or plain git diff)",
    ));
    checks.push(tool(
        "gh",
        Level::Warn,
        "PR badges and `wt pr`",
        "install the GitHub CLI and run `gh auth login`",
    ));
    if cfg!(target_os = "linux") {
        checks.push(tool(
            "notify-send",
            Level::Warn,
            "desktop notifications",
            "install libnotify",
        ));
    }
    checks.push(check_sound_player());
    checks.push(check_wt_scripts());
    checks.push(check_claude_hooks());
    checks.push(check_theme());
    checks
}

fn exit_code(checks: &[Check]) -> i32 {
    if checks.iter().any(|c| c.level == Level::Fail) {
        1
    } else {
        0
    }
}

pub fn run_cli() -> i32 {
    let checks = run_checks();
    println!("waymux doctor ({})\n", env!("CARGO_PKG_VERSION"));
    for c in &checks {
        let mark = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("  [{mark}] {:<12} {}", c.name, c.detail);
        if c.level != Level::Ok
            && let Some(fix) = &c.fix
        {
            println!("         {:<12} fix: {fix}", "");
        }
    }
    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
    println!("\n{fails} failing, {warns} warnings");
    exit_code(&checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_code_fails_only_on_required_checks() {
        let warn = Check::problem(Level::Warn, "wm", "missing", "install");
        let fail = Check::problem(Level::Fail, "tmux", "missing", "install");
        assert_eq!(exit_code(&[Check::ok("git", "/usr/bin/git"), warn]), 0);
        assert_eq!(exit_code(&[fail]), 1);
    }
}
