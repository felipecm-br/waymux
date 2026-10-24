pub mod actions;
pub mod diff;
pub mod help;
pub mod items;
pub mod notes;
pub mod preview;

use crate::presets::materialize_cockpit_toml;
use crate::sys::shell_quote;
use crate::theme::ThemePalette;
use crate::tmux_cmd::{
    tmux_display_msg, tmux_get_global_opt, tmux_out, tmux_set_global_opt, tmux_status,
};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};

fn is_stdout_tty() -> bool {
    unsafe { nix::libc::isatty(std::io::stdout().as_raw_fd()) == 1 }
}

fn resolve_target_path(item: &str, orig_sess: &str) -> String {
    let first_line = item.lines().next().unwrap_or("");
    let mut t_path = String::new();
    let mut t_sess = String::new();

    if let Some(stripped) = first_line.strip_prefix('#') {
        t_sess = stripped.split_whitespace().next().unwrap_or("").to_string();
    } else {
        let cols: Vec<&str> = first_line.split('\t').collect();
        if let Some(s) = cols.get(3) {
            t_sess = (*s).to_string();
        }
        if let Some(p) = cols.get(5)
            && !p.is_empty()
            && Path::new(p).is_dir()
        {
            t_path = (*p).to_string();
        }
    }

    if t_path.is_empty() && !t_sess.is_empty() {
        let target = format!("{}:", t_sess);
        t_path = tmux_out(&[
            "display-message",
            "-t",
            &target,
            "-p",
            "#{pane_current_path}",
        ])
        .unwrap_or_default();
    }

    if (t_path.is_empty() || !Path::new(&t_path).is_dir()) && !orig_sess.is_empty() {
        let target = format!("{}:", orig_sess);
        t_path = tmux_out(&[
            "display-message",
            "-t",
            &target,
            "-p",
            "#{pane_current_path}",
        ])
        .unwrap_or_default();
    }

    if t_path.is_empty() || !Path::new(&t_path).is_dir() {
        t_path = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
    }

    t_path
}

fn compute_start_idx(
    items_str: &str,
    saved_sess: &str,
    saved_idx: &str,
    orig_sess: &str,
    orig_win: &str,
) -> usize {
    if !saved_sess.is_empty() {
        let mut n = 0usize;
        for line in items_str.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let s = cols.get(3).copied().unwrap_or("");
            let i = cols.get(1).copied().unwrap_or("");
            if !saved_idx.is_empty() && saved_idx != "-" {
                if s == saved_sess && i == saved_idx {
                    return n;
                }
            } else if s == saved_sess {
                return n;
            }
            n += 1;
        }
    } else {
        if let Some(n) = first_waiting_row(items_str) {
            return n;
        }
        let mut n = 0usize;
        for line in items_str.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let s = cols.get(3).copied().unwrap_or("");
            let i = cols.get(1).copied().unwrap_or("");
            if s == orig_sess && i == orig_win {
                return n;
            }
            n += 1;
        }
    }
    0
}

/// Index of the first row that needs the human (column 6 is the raw state): an agent waiting
/// on you, else finished work nobody has reviewed. Rows arrive sorted by service class and
/// wait time, so this is the next thing to serve.
fn first_waiting_row(items_str: &str) -> Option<usize> {
    let states: Vec<&str> = items_str
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').nth(6).unwrap_or(""))
        .collect();
    states
        .iter()
        .position(|st| crate::triage::service_class(st).is_some())
        .or_else(|| states.iter().position(|st| *st == "review"))
}

/// `S` ships (merge → push → delete branch), which cannot be undone: ask first,
/// exactly like `S` in the awt popup.
fn ship_cli_args(branch: String) -> Vec<String> {
    vec!["ship".to_string(), branch, "--confirm".to_string()]
}

fn find_wm_bin() -> String {
    if let Ok(home) = std::env::var("HOME") {
        let wm = format!("{}/.local/bin/wm", home);
        if Path::new(&wm).is_file() {
            return wm;
        }
        let mm = format!("{}/.local/bin/mm", home);
        if Path::new(&mm).is_file() {
            return mm;
        }
    }
    "wm".to_string()
}

fn find_lazygitrs_bin() -> String {
    if let Ok(home) = std::env::var("HOME") {
        let l1 = format!("{}/.local/bin/lazygitrs", home);
        if Path::new(&l1).is_file() {
            return l1;
        }
        let l2 = format!("{}/.cargo/bin/lazygitrs", home);
        if Path::new(&l2).is_file() {
            return l2;
        }
    }
    "lazygitrs".to_string()
}

const COCKPIT_BADGE: &str = " 󱂬 ";

pub fn run_cockpit(args: &[String]) -> i32 {
    let is_fullscreen = args.iter().any(|a| a == "--fullscreen");
    let exe_str = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "waymux".to_string());

    if is_fullscreen {
        if !is_stdout_tty() {
            let cmd = format!("{} cockpit --fullscreen", shell_quote(&exe_str));
            let _ = tmux_status(&["split-window", "-Z", &cmd]);
            return 0;
        }
    } else if std::env::var("TMUX_POPUP").unwrap_or_default().is_empty() {
        let orig_sess = tmux_out(&["display-message", "-p", "#{session_name}"]).unwrap_or_default();
        let orig_win = tmux_out(&["display-message", "-p", "#{window_index}"]).unwrap_or_default();
        let theme = ThemePalette::load();
        let popup_text_opt = tmux_get_global_opt("@POPUP_TEXT_COLOR");
        let text_col = std::env::var("TMUX_POPUP_TEXT_COLOR")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| (!popup_text_opt.is_empty()).then_some(popup_text_opt))
            .unwrap_or_else(|| "default".into());
        let popup_cmd = format!(
            "TMUX_POPUP=1 TMUX_ORIGIN_SESSION={} TMUX_ORIGIN_WINDOW={} {} cockpit",
            shell_quote(&orig_sess),
            shell_quote(&orig_win),
            shell_quote(&exe_str)
        );
        let border_fg = format!("fg={}", theme.orange);
        let text_fg = format!("fg={}", text_col);
        let mut popup_args = vec![
            "display-popup",
            "-S",
            &border_fg,
            "-s",
            &text_fg,
            "-b",
            "rounded",
            "-T",
            COCKPIT_BADGE,
        ];
        // @border_gradient holds a Waymaker gradient palette: the popup opens
        // borderless and Waymaker draws a gradient frame carrying the badge.
        let gradient = tmux_get_global_opt("@border_gradient");
        let gradient_env = format!("WM_BORDER_GRADIENT={}", gradient.trim());
        let title_env = format!("WM_BORDER_TITLE={}", COCKPIT_BADGE);
        if !gradient.trim().is_empty() {
            popup_args.extend(["-B", "-e", &gradient_env, "-e", &title_env]);
        }
        popup_args.extend(["-w", "85%", "-h", "75%", "-E", &popup_cmd]);
        let _ = tmux_status(&popup_args);
        return 0;
    }

    let spinner_opt = tmux_get_global_opt("@ai_agent_spinner");
    let spinner_name = if spinner_opt.is_empty() {
        "minidot"
    } else {
        &spinner_opt
    };

    let orig_sess = std::env::var("TMUX_ORIGIN_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| tmux_out(&["display-message", "-p", "#S"]))
        .unwrap_or_default();
    let orig_win = std::env::var("TMUX_ORIGIN_WINDOW")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| tmux_out(&["display-message", "-p", "#I"]))
        .unwrap_or_default();

    let mut saved_sess = String::new();
    let mut saved_idx = String::new();
    let toml_path = materialize_cockpit_toml();
    let wm_bin = find_wm_bin();

    let seg_bg = tmux_get_global_opt("@SEGMENT_BG");
    let fg_opt = tmux_get_global_opt("@FG");
    let accent_opt = tmux_get_global_opt("@ACCENT_COLOR");

    let color_spec = std::env::var("TMUX_COLOR_SPEC")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if !seg_bg.is_empty() && !fg_opt.is_empty() && !accent_opt.is_empty() {
                format!(
                    "list-border:{},group-header:{},selected-prefix:{},selected-fg:{}",
                    seg_bg, fg_opt, accent_opt, fg_opt
                )
            } else {
                "list-border:239,group-header:245,selected-prefix:214,selected-fg:223".to_string()
            }
        });
    let spinner_color = std::env::var("TMUX_SPINNER_COLOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| (!accent_opt.is_empty()).then_some(accent_opt))
        .unwrap_or_else(|| "214".to_string());

    let origin = format!("{}:{}", orig_sess, orig_win);
    loop {
        let items_str = items::generate_items(false, &orig_sess, &orig_win);
        let start_idx =
            compute_start_idx(&items_str, &saved_sess, &saved_idx, &orig_sess, &orig_win);

        let mut child = match Command::new(&wm_bin)
            .args([
                "--config",
                &toml_path.to_string_lossy(),
                &format!("results.spinner={}", spinner_name),
                "--pos",
                &start_idx.to_string(),
                "--color",
                &format!("spinner:{}", spinner_color),
                "--color",
                &color_spec,
                "--group-prefix",
                "#",
            ])
            .env(crate::keylog::ORIGIN_ENV, &origin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return 1,
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(items_str.as_bytes());
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(_) => return 0,
        };

        let chosen = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();

        if chosen.is_empty() {
            crate::keylog::record("cockpit", "esc", "", "", &origin);
            return 0;
        }

        if let Some(raw_item) = chosen.strip_prefix("__CREATE_WINDOW__") {
            let _ = actions::create_window_from_raw(raw_item);
            return 0;
        } else if let Some(raw_item) = chosen.strip_prefix("__NEW_WORKTREE__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let _ = std::env::set_current_dir(&target_path);
            return crate::wt::run_cli(&["new".to_string()]);
        } else if let Some(raw_item) = chosen.strip_prefix("__SHIP_WORKTREE__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let branch = Command::new("git")
                .args(["-C", &target_path, "rev-parse", "--abbrev-ref", "HEAD"])
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if branch.is_empty() || matches!(branch.as_str(), "main" | "master" | "HEAD") {
                tmux_display_msg(&format!(
                    "Cannot ship default branch '{}' (not a feature worktree)",
                    branch
                ));
                return 0;
            }
            let _ = std::env::set_current_dir(&target_path);
            return crate::wt::run_cli(&ship_cli_args(branch));
        } else if let Some(raw_item) = chosen.strip_prefix("__SWEEP_WORKTREES__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let _ = std::env::set_current_dir(&target_path);
            return crate::wt::run_cli(&["sweep".to_string()]);
        } else if chosen.starts_with("__REAP_AGENTS__") {
            let target_path = resolve_target_path("", &orig_sess);
            let _ = std::env::set_current_dir(&target_path);
            return crate::wt::run_cli(&["reap".to_string()]);
        } else if let Some(raw_item) = chosen.strip_prefix("__OPEN_PR__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let first = raw_item.lines().next().unwrap_or("");
            let cols: Vec<&str> = first.split('\t').collect();
            saved_sess = cols.get(3).copied().unwrap_or("").to_string();
            saved_idx = cols.get(1).copied().unwrap_or("").to_string();

            let _ = Command::new("sh")
                .args([
                    "-c",
                    "cd \"$1\" 2>/dev/null && gh pr view --web 2>/dev/null || tmux display-message 'No pull request found for current branch'",
                    "sh",
                    &target_path,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            continue;
        } else if let Some(raw_item) = chosen.strip_prefix("__OPEN_PR_CHECKS__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let first = raw_item.lines().next().unwrap_or("");
            let cols: Vec<&str> = first.split('\t').collect();
            saved_sess = cols.get(3).copied().unwrap_or("").to_string();
            saved_idx = cols.get(1).copied().unwrap_or("").to_string();

            let _ = Command::new("sh")
                .args([
                    "-c",
                    "cd \"$1\" 2>/dev/null && gh pr checks --web 2>/dev/null || tmux display-message 'No PR checks found for current branch'",
                    "sh",
                    &target_path,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            continue;
        } else if chosen.starts_with("__JUMP_LAST_AGENT__") {
            let last_target = tmux_get_global_opt("@cockpit_last_agent");
            let current_target = format!("{}:{}", orig_sess, orig_win);
            if current_target != ":" {
                tmux_set_global_opt("@cockpit_last_agent", &current_target);
            }
            if !last_target.is_empty() {
                if !tmux_status(&["switch-client", "-t", &last_target]) {
                    let _ = tmux_status(&["switch-client", "-l"]);
                }
            } else {
                let _ = tmux_status(&["switch-client", "-l"]);
            }
            return 0;
        } else if let Some(raw_item) = chosen
            .strip_prefix("__LAZYGIT__")
            .or_else(|| chosen.strip_prefix("__PATCH_MODE__"))
        {
            let is_patch = chosen.starts_with("__PATCH_MODE__");
            let target_path = resolve_target_path(raw_item, &orig_sess);
            let first = raw_item.lines().next().unwrap_or("");
            let cols: Vec<&str> = first.split('\t').collect();
            let target_sess = cols.get(3).copied().unwrap_or("").to_string();
            let target_idx = cols.get(1).copied().unwrap_or("").to_string();

            let is_git = Command::new("git")
                .args(["-C", &target_path, "rev-parse", "--is-inside-work-tree"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if !is_git {
                tmux_display_msg(&format!("Not in a git repository: {}", target_path));
                if is_patch {
                    continue;
                }
                return 0;
            }

            let porcelain = Command::new("git")
                .args(["-C", &target_path, "status", "--porcelain"])
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            if is_patch && porcelain.is_empty() {
                tmux_display_msg(&format!(
                    "Working tree is clean: nothing to patch in {}",
                    target_path
                ));
                continue;
            }

            saved_sess = target_sess;
            saved_idx = target_idx;
            let review_before = items::review_snapshot(Path::new(&target_path));
            let review_started = std::time::Instant::now();

            let lzg_bin = find_lazygitrs_bin();
            let mut lzg_args = vec!["-d", "-c", "popup"];
            if !is_patch && porcelain.is_empty() {
                lzg_args.push("--commits");
            }

            let rc = Command::new(&lzg_bin)
                .args(&lzg_args)
                .current_dir(&target_path)
                .env("LAZYGITRS_COCKPIT", "1")
                .status()
                .ok()
                .and_then(|s| s.code())
                .unwrap_or(0);
            items::finish_review(
                Path::new(&target_path),
                &review_before,
                review_started.elapsed(),
            );

            if rc == 10 {
                continue;
            }
            return 0;
        } else if let Some(raw_item) = chosen.strip_prefix("__SWITCH_AWT__") {
            let target_path = resolve_target_path(raw_item, &orig_sess);
            if let Ok(home) = std::env::var("HOME") {
                let awt_popup = format!("{}/.config/tmux/awt-popup.sh", home);
                if Path::new(&awt_popup).is_file() {
                    let _ = Command::new(&awt_popup).arg(&target_path).status();
                    return 0;
                }
            }
            return 0;
        } else if chosen == "__SWITCH_SESSION__" {
            if let Ok(home) = std::env::var("HOME") {
                let sp = format!("{}/.config/tmux/session-picker.sh", home);
                if Path::new(&sp).is_file() {
                    let mut cmd = Command::new(&sp);
                    if is_fullscreen {
                        cmd.arg("--fullscreen");
                    }
                    let _ = cmd.status();
                    return 0;
                }
            }
            return 0;
        } else {
            if chosen.contains("no active AI agents") {
                return 0;
            }
            let first = chosen.lines().next().unwrap_or("");
            let cols: Vec<&str> = first.split('\t').collect();
            let session = cols.get(3).copied().unwrap_or("");
            let idx = cols.get(1).copied().unwrap_or("");
            let state = cols.get(6).copied().unwrap_or("");
            crate::keylog::record(
                "cockpit",
                "enter",
                state,
                &format!("{}:{}", session, idx),
                &origin,
            );
            if !session.is_empty() && !idx.is_empty() && idx != "-" {
                let current_target = format!("{}:{}", orig_sess, orig_win);
                if current_target != ":" {
                    tmux_set_global_opt("@cockpit_last_agent", &current_target);
                }
                let target = format!("{}:{}", session, idx);
                let _ = tmux_status(&["switch-client", "-t", &target]);
            }
            return 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(idx: &str, sess: &str, state: &str) -> String {
        format!("t\t{idx}\tname\t{sess}\t%1\t/tmp\t{state}\t-\t-\t-\t-\t-")
    }

    #[test]
    fn test_start_idx_prefers_first_waiting_agent() {
        let items = [
            "#  api".to_string(),
            row("2", "api", "permission"),
            "#  dotfiles".to_string(),
            row("0", "dotfiles", "busy"),
            row("1", "dotfiles", "idle"),
        ]
        .join("\n");
        // Nothing saved: the cursor starts on the agent waiting for you, not your window.
        assert_eq!(compute_start_idx(&items, "", "", "dotfiles", "1"), 0);
        // A saved selection (returning from lazygitrs) still wins.
        assert_eq!(
            compute_start_idx(&items, "dotfiles", "1", "dotfiles", "1"),
            2
        );
    }

    #[test]
    fn test_start_idx_falls_back_to_origin_window() {
        let items = [row("0", "s", "busy"), row("1", "s", "idle")].join("\n");
        assert_eq!(compute_start_idx(&items, "", "", "s", "1"), 1);
        assert_eq!(first_waiting_row(&items), None);
    }

    #[test]
    fn test_start_idx_prefers_waiting_then_unreviewed() {
        let review_only = [row("0", "s", "busy"), row("1", "s", "review")].join("\n");
        assert_eq!(compute_start_idx(&review_only, "", "", "s", "0"), 1);

        let both = [
            row("0", "s", "review"),
            row("1", "s", "busy"),
            row("2", "s", "question"),
        ]
        .join("\n");
        assert_eq!(first_waiting_row(&both), Some(2));
    }

    #[test]
    fn test_ship_asks_for_confirmation() {
        assert_eq!(
            ship_cli_args("feat/x".to_string()),
            vec!["ship", "feat/x", "--confirm"]
        );
    }
}
