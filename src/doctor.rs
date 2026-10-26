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

fn check_daemon_health(
    port: u16,
    resp: Option<(u16, serde_json::Value)>,
    cli_version: &str,
) -> Check {
    match resp {
        Some((200, body)) => match body.get("version").and_then(|v| v.as_str()) {
            Some(ver) if ver == cli_version => {
                Check::ok("daemon", format!("healthy on 127.0.0.1:{port} (v{ver})"))
            }
            Some(ver) => Check::problem(
                Level::Warn,
                "daemon",
                &format!("running v{ver} on 127.0.0.1:{port}, but CLI is v{cli_version}"),
                "systemctl --user restart waymux",
            ),
            None => Check::problem(
                Level::Warn,
                "daemon",
                &format!("running older binary on 127.0.0.1:{port} (CLI is v{cli_version})"),
                "systemctl --user restart waymux",
            ),
        },
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

fn check_daemon() -> Check {
    let port = rpc_client::daemon_port();
    let resp = rpc_client::http_get_json(port, "/health", 1500);
    check_daemon_health(port, resp, env!("CARGO_PKG_VERSION"))
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
            "install tmux 3.2 or newer from your package manager",
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

fn check_config_at(path: Option<&str>) -> Check {
    let Some(path) = path else {
        return Check::ok("config", "built-in defaults (no config.toml found)");
    };
    let fix = format!("fix or remove {path} (see config/default.toml)");
    match std::fs::read_to_string(path) {
        Err(e) => Check::problem(Level::Fail, "config", &format!("{path}: {e}"), &fix),
        Ok(content) => match toml::from_str::<crate::config::Config>(&content) {
            Ok(_) => Check::ok("config", path),
            Err(e) => Check::problem(
                Level::Fail,
                "config",
                &format!("{path}: {}", e.message()),
                &fix,
            ),
        },
    }
}

fn check_config() -> Check {
    let path = crate::config::Config::default_path();
    check_config_at(path.as_deref())
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

/// Directive keys (`WatchdogSec`, `SystemCallFilter`, ...) of the shipped unit that `installed`
/// lacks. Values are not compared, so a customized unit passes as long as no setting is missing.
fn missing_unit_directives(installed: &str, shipped: &str) -> Vec<String> {
    fn keys(unit: &str) -> Vec<&str> {
        unit.lines()
            .map(str::trim)
            .filter(|l| !l.starts_with(['#', ';', '[']))
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
            .collect()
    }
    let have = keys(installed);
    let mut missing: Vec<String> = keys(shipped)
        .into_iter()
        .filter(|k| !have.contains(k))
        .map(str::to_string)
        .collect();
    missing.dedup();
    missing
}

fn check_systemd_unit() -> Check {
    let config = std::env::var("XDG_CONFIG_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
        .unwrap_or_default();
    let path = format!("{config}/systemd/user/waymux.service");
    let fix = format!(
        "waymux init --systemd > {path} && systemctl --user daemon-reload && systemctl --user restart waymux"
    );
    match std::fs::read_to_string(&path) {
        Err(_) => Check {
            level: Level::Ok,
            name: "systemd unit",
            detail: format!("not installed at {path}"),
            fix: Some(format!("optional: {fix}")),
        },
        Ok(installed) => {
            let missing = missing_unit_directives(&installed, crate::init::SYSTEMD_UNIT);
            if missing.is_empty() {
                Check::ok("systemd unit", path)
            } else {
                Check::problem(
                    Level::Warn,
                    "systemd unit",
                    &format!("older than this release, missing {}", missing.join(", ")),
                    &fix,
                )
            }
        }
    }
}

fn run_checks() -> Vec<Check> {
    let mut checks = vec![check_daemon(), check_token(), check_config(), check_tmux()];
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
    if find_in_path("lazygitrs").is_none() && find_in_path("bat").is_none() {
        checks.push(tool(
            "delta",
            Level::Warn,
            "side-by-side diffs",
            "install git-delta (falls back to bat or plain git diff)",
        ));
    }
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
    if cfg!(target_os = "linux") {
        checks.push(check_systemd_unit());
    }
    checks.push(check_sound_player());
    checks.push(check_wt_scripts());
    checks.push(check_claude_hooks());
    checks.push(check_theme());
    checks
}

fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|c| c.level == Level::Fail))
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
    fn test_missing_unit_directives_ignores_values_and_comments() {
        let shipped =
            "[Service]\n# WatchdogSec=1s\nWatchdogSec=30s\nUMask=0077\nRestart=on-failure\n";
        let custom = "[Service]\nRestart=always\nUMask=0022\n";
        assert_eq!(missing_unit_directives(custom, shipped), ["WatchdogSec"]);
        assert_eq!(
            missing_unit_directives(shipped, shipped),
            Vec::<String>::new()
        );
        assert_eq!(
            missing_unit_directives(crate::init::SYSTEMD_UNIT, crate::init::SYSTEMD_UNIT),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_exit_code_fails_only_on_required_checks() {
        let warn = Check::problem(Level::Warn, "wm", "missing", "install");
        let fail = Check::problem(Level::Fail, "tmux", "missing", "install");
        assert_eq!(exit_code(&[Check::ok("git", "/usr/bin/git"), warn]), 0);
        assert_eq!(exit_code(&[fail]), 1);
    }

    #[test]
    fn test_check_config_reports_first_parse_error() {
        assert_eq!(check_config_at(None).level, Level::Ok);
        let dir = std::env::temp_dir().join(format!("waymux-doc-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "port = 4040\n").unwrap();
        assert_eq!(check_config_at(path.to_str()).level, Level::Ok);

        std::fs::write(&path, "flow_log_hok = \"/bin/true\"\n").unwrap();
        let bad = check_config_at(path.to_str());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(bad.level, Level::Fail);
        assert!(
            bad.detail.contains("flow_log_hok"),
            "detail should name the unknown key: {}",
            bad.detail
        );
    }

    #[test]
    fn test_check_daemon_health_detects_version_mismatch() {
        let ok = check_daemon_health(
            4040,
            Some((
                200,
                serde_json::json!({"status": "healthy", "version": "0.2.4"}),
            )),
            "0.2.4",
        );
        assert_eq!(ok.level, Level::Ok);
        assert!(ok.detail.contains("v0.2.4"), "{}", ok.detail);

        let mismatch = check_daemon_health(
            4040,
            Some((
                200,
                serde_json::json!({"status": "healthy", "version": "0.2.3"}),
            )),
            "0.2.4",
        );
        assert_eq!(mismatch.level, Level::Warn);
        assert!(
            mismatch.detail.contains("running v0.2.3"),
            "{}",
            mismatch.detail
        );

        let legacy = check_daemon_health(
            4040,
            Some((200, serde_json::json!({"status": "healthy"}))),
            "0.2.4",
        );
        assert_eq!(legacy.level, Level::Warn);

        let down = check_daemon_health(4040, None, "0.2.4");
        assert_eq!(down.level, Level::Fail);
    }
}
