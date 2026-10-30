//! `waymux quota --popup`: the visual report in a tmux popup, with one-key actions:
//! `Enter` launches the recommended agent (`[quota] launch`), `a` picks one (`[quota] pick`),
//! `r` refreshes every provider, `q`/`u`/`Esc` close. Outside tmux it runs in the terminal.

use nix::sys::termios::{self, LocalFlags, SetArg, SpecialCharacterIndices, Termios};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use super::render::{self, Colors};
use super::{Snapshot, quota_config};
use crate::config::QuotaConfig;
use crate::sys::shell_quote;
use crate::theme::{BOLD, RESET};
use crate::tmux_cmd::{tmux_get_global_opt, tmux_out, tmux_status};

const BADGE: &str = " 󰓅 ";
const DEFAULT_LAUNCH: &str = "exec \"$WAYMUX_QUOTA_AGENT\"";
/// Settings a wrapper may set in its own environment; the popup's process starts with the
/// tmux server's, so they are passed with `display-popup -e`.
const FORWARDED_ENV: [&str; 5] = [
    "WAYMUX_QUOTA_PROVIDERS",
    "WAYMUX_QUOTA_PREFERENCE",
    "WAYMUX_QUOTA_SESSION_RATE",
    "WAYMUX_QUOTA_STATE_DIR",
    "CLAUDE_CONFIG_DIR",
];

pub fn run(dir: Option<&str>) -> i32 {
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
    if in_tmux && !in_popup {
        return open_popup(&dir);
    }
    if !dir.is_empty() {
        let _ = std::env::set_current_dir(&dir);
    }
    run_loop(&dir, &quota_config())
}

fn open_popup(dir: &str) -> i32 {
    let exe = std::env::current_exe()
        .map_or_else(|_| "waymux".into(), |p| p.to_string_lossy().to_string());
    let origin = tmux_out(&["display-message", "-p", "#{session_id}"]).unwrap_or_default();
    let theme = crate::theme::ThemePalette::load();
    let text_opt = tmux_get_global_opt("@POPUP_TEXT_COLOR");
    let text_col = std::env::var("TMUX_POPUP_TEXT_COLOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| (!text_opt.is_empty()).then_some(text_opt))
        .unwrap_or_else(|| "default".into());
    let cmd = popup_command(&exe, &origin, dir);
    let border = format!("fg={}", theme.cyan);
    let text = format!("fg={text_col}");
    let env = forwarded_env(|name| std::env::var(name).ok());
    let mut args = vec![
        "display-popup",
        "-S",
        &border,
        "-s",
        &text,
        "-b",
        "rounded",
        "-T",
        BADGE,
        "-d",
        dir,
    ];
    for pair in &env {
        args.extend(["-e", pair.as_str()]);
    }
    args.extend(["-E", "-w", "75%", "-h", "60%", &cmd]);
    i32::from(!tmux_status(&args))
}

fn forwarded_env(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    FORWARDED_ENV
        .iter()
        .filter_map(|name| get(name).map(|v| format!("{name}={v}")))
        .collect()
}

fn popup_command(exe: &str, origin: &str, dir: &str) -> String {
    format!(
        "TMUX_POPUP=1 TMUX_ORIGIN_SESSION={} {} quota --popup {}",
        shell_quote(origin),
        shell_quote(exe),
        shell_quote(dir)
    )
}

/// Keys read one at a time, unechoed, with the cursor hidden; restored on drop.
struct RawMode {
    saved: Option<Termios>,
}

impl RawMode {
    fn enter() -> Self {
        let stdin = std::io::stdin();
        let saved = termios::tcgetattr(stdin.as_fd()).ok();
        if let Some(orig) = &saved {
            let mut raw = orig.clone();
            raw.local_flags
                .remove(LocalFlags::ICANON | LocalFlags::ECHO);
            raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
            raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
            let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw);
        }
        print!("\x1b[?25l");
        let _ = std::io::stdout().flush();
        Self { saved }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(orig) = &self.saved {
            let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, orig);
        }
        print!("\x1b[?25h");
        let _ = std::io::stdout().flush();
    }
}

/// Rows and columns of the controlling terminal (24×80 when unknown).
fn term_size() -> (usize, usize) {
    let size = std::fs::File::open("/dev/tty")
        .ok()
        .and_then(|tty| Command::new("stty").arg("size").stdin(tty).output().ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut parts = size.split_whitespace().map(|v| v.parse::<usize>().ok());
    let rows = parts.next().flatten().filter(|&r| r >= 10).unwrap_or(24);
    let cols = parts.next().flatten().filter(|&c| c >= 20).unwrap_or(80);
    (rows, cols)
}

fn footer(c: &Colors, has_pick: bool) -> String {
    let key =
        |k: &str, label: &str| format!("{BOLD}{}{k}{RESET} {}{label}{RESET}", c.cyan, c.muted);
    let sep = format!(" {}│{RESET} ", c.muted);
    let mut items = vec![
        key("Enter", "Launch"),
        format!("{BOLD}\x1b[97m⠛{RESET} {}Recommended{RESET}", c.muted),
    ];
    if has_pick {
        items.push(key("a", "Models"));
    }
    items.push(key("r", "Refresh"));
    items.push(key("Esc", "Close"));
    format!(" {}", items.join(&sep))
}

fn run_loop(dir: &str, cfg: &QuotaConfig) -> i32 {
    let colors = Colors::load();
    let raw = RawMode::enter();
    let mut stdout = std::io::stdout();
    loop {
        let snap = Snapshot::collect(false, false);
        let (rows, cols) = term_size();
        let _ = write!(
            stdout,
            "\x1b[H\x1b[2J\n{}\n",
            render::visual(&snap, &colors)
        );
        // Cockpit-style separator and footer pinned to the bottom rows.
        let _ = write!(
            stdout,
            "\x1b[{};1H{}{}{RESET}\x1b[{rows};1H{}",
            rows - 1,
            colors.muted,
            "─".repeat(cols - 1),
            footer(&colors, cfg.pick.is_some())
        );
        let _ = stdout.flush();

        let mut key = [0u8; 1];
        if std::io::stdin().read(&mut key).unwrap_or(0) == 0 {
            return 0;
        }
        match key[0] {
            b'\r' | b'\n' => {
                drop(raw);
                let cmd = cfg.launch.as_deref().unwrap_or(DEFAULT_LAUNCH);
                return launch(cmd, &snap.recommend, dir);
            }
            b'a' | b'A' if cfg.pick.is_some() => {
                drop(raw);
                print!("\x1b[H\x1b[2J");
                let _ = stdout.flush();
                let pick = cfg.pick.as_deref().unwrap_or_default();
                let choice = Command::new("sh")
                    .args(["-c", pick])
                    .env("WAYMUX_QUOTA_AGENT", &snap.recommend)
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                if choice.is_empty() {
                    return 0;
                }
                return launch(&choice, &snap.recommend, dir);
            }
            b'r' | b'R' => {
                let _ = write!(
                    stdout,
                    "\x1b[{rows};1H\x1b[2K  {}⟳ refreshing quota telemetry…{RESET}",
                    colors.muted
                );
                let _ = stdout.flush();
                super::refresh();
            }
            0x1b | b'q' | b'Q' | b'u' | b'U' => return 0,
            _ => {}
        }
    }
}

/// Runs `cmd` in a new window of the session the popup was opened from (in this terminal
/// outside tmux), with `$WAYMUX_QUOTA_AGENT` set to the recommended agent.
fn launch(cmd: &str, agent: &str, dir: &str) -> i32 {
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    if !in_tmux {
        let err = Command::new("sh")
            .args(["-c", cmd])
            .env("WAYMUX_QUOTA_AGENT", agent)
            .exec();
        eprintln!("waymux quota: {err}");
        return 1;
    }
    let env = format!("WAYMUX_QUOTA_AGENT={agent}");
    let origin = std::env::var("TMUX_ORIGIN_SESSION").unwrap_or_default();
    let target = format!("{origin}:");
    let mut args = vec!["new-window"];
    if !origin.is_empty() {
        args.extend(["-t", &target]);
    }
    if !dir.is_empty() {
        args.extend(["-c", dir]);
    }
    args.extend(["-e", &env, cmd]);
    i32::from(!tmux_status(&args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_popup_command_quotes_every_value() {
        let cmd = popup_command("/opt/my bin/waymux", "$1", "/tmp/it's here");
        assert_eq!(
            cmd,
            "TMUX_POPUP=1 TMUX_ORIGIN_SESSION='$1' '/opt/my bin/waymux' quota --popup '/tmp/it'\\''s here'"
        );
    }

    #[test]
    fn test_popup_receives_the_wrapper_settings_that_are_set() {
        let env = forwarded_env(|name| match name {
            "WAYMUX_QUOTA_STATE_DIR" => Some("/s".into()),
            "WAYMUX_QUOTA_PROVIDERS" => Some("a\nb".into()),
            _ => None,
        });
        assert_eq!(
            env,
            vec!["WAYMUX_QUOTA_PROVIDERS=a\nb", "WAYMUX_QUOTA_STATE_DIR=/s"]
        );
    }

    #[test]
    fn test_footer_offers_models_only_with_a_pick_command() {
        let c = Colors::basic();
        let strip = |s: String| crate::theme::strip_ansi(&s);
        assert!(strip(footer(&c, true)).contains("a Models"));
        assert!(!strip(footer(&c, false)).contains("Models"));
        assert!(strip(footer(&c, false)).contains("Esc Close"));
    }
}
