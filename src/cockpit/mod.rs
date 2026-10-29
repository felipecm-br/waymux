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
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn is_stdout_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Fields extracted from a Cockpit row (either a `# session` group header or a 12-column TSV row).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RowTarget {
    raw: String,
    session: String,
    idx: String,
    path: String,
    state: String,
}

impl RowTarget {
    fn parse(raw: &str) -> Self {
        let first_line = raw.lines().next().unwrap_or("");
        if let Some(stripped) = first_line.strip_prefix('#') {
            let session = stripped.split_whitespace().next().unwrap_or("").to_string();
            return Self {
                raw: raw.to_string(),
                session,
                ..Self::default()
            };
        }
        let cols: Vec<&str> = first_line.split('\t').collect();
        Self {
            raw: raw.to_string(),
            idx: cols.get(1).copied().unwrap_or("").to_string(),
            session: cols.get(3).copied().unwrap_or("").to_string(),
            path: cols.get(5).copied().unwrap_or("").to_string(),
            state: cols.get(6).copied().unwrap_or("").to_string(),
        }
    }
}

/// Action selected in the Waymaker Cockpit picker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Cancel,
    Noop,
    CreateWindow(String),
    NewWorktree(RowTarget),
    ShipWorktree(RowTarget),
    SweepWorktrees(RowTarget),
    ReapAgents,
    OpenPr { row: RowTarget, checks: bool },
    JumpLastAgent,
    LazyGit { row: RowTarget, patch_mode: bool },
    SwitchAwt(RowTarget),
    SwitchSession,
    Jump(RowTarget),
}

fn parse_action(chosen: &str) -> Action {
    if chosen.is_empty() {
        Action::Cancel
    } else if let Some(raw_item) = chosen.strip_prefix("__CREATE_WINDOW__") {
        Action::CreateWindow(raw_item.to_string())
    } else if let Some(raw_item) = chosen.strip_prefix("__NEW_WORKTREE__") {
        Action::NewWorktree(RowTarget::parse(raw_item))
    } else if let Some(raw_item) = chosen.strip_prefix("__SHIP_WORKTREE__") {
        Action::ShipWorktree(RowTarget::parse(raw_item))
    } else if let Some(raw_item) = chosen.strip_prefix("__SWEEP_WORKTREES__") {
        Action::SweepWorktrees(RowTarget::parse(raw_item))
    } else if chosen.starts_with("__REAP_AGENTS__") {
        Action::ReapAgents
    } else if let Some(raw_item) = chosen.strip_prefix("__OPEN_PR__") {
        Action::OpenPr {
            row: RowTarget::parse(raw_item),
            checks: false,
        }
    } else if let Some(raw_item) = chosen.strip_prefix("__OPEN_PR_CHECKS__") {
        Action::OpenPr {
            row: RowTarget::parse(raw_item),
            checks: true,
        }
    } else if chosen.starts_with("__JUMP_LAST_AGENT__") {
        Action::JumpLastAgent
    } else if let Some(raw_item) = chosen.strip_prefix("__LAZYGIT__") {
        Action::LazyGit {
            row: RowTarget::parse(raw_item),
            patch_mode: false,
        }
    } else if let Some(raw_item) = chosen.strip_prefix("__PATCH_MODE__") {
        Action::LazyGit {
            row: RowTarget::parse(raw_item),
            patch_mode: true,
        }
    } else if let Some(raw_item) = chosen.strip_prefix("__SWITCH_AWT__") {
        Action::SwitchAwt(RowTarget::parse(raw_item))
    } else if chosen == "__SWITCH_SESSION__" {
        Action::SwitchSession
    } else if chosen.contains("no active AI agents") {
        Action::Noop
    } else {
        Action::Jump(RowTarget::parse(chosen))
    }
}

/// Whether the Cockpit loop should reopen the picker (optionally saving the selected row) or exit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flow {
    Reopen(Option<(String, String)>),
    Exit(i32),
}

/// Side-effecting operations used by [`execute`], abstracted so unit tests can drive every branch
/// without spawning tmux, git, `wm`, `gh`, or `lazygitrs`.
trait CockpitOps {
    fn is_dir(&self, path: &str) -> bool;
    fn tmux_pane_path(&self, session: &str) -> String;
    fn current_dir(&self) -> String;
    fn keylog(&self, key: &str, state: &str, target: &str, origin: &str);
    fn create_window(&self, raw_item: &str);
    fn run_wt(&self, dir: &str, args: &[String]) -> i32;
    fn git_branch(&self, dir: &str) -> String;
    /// `None` when `dir` is not inside a git work tree; `Some(porcelain)` otherwise.
    fn git_porcelain(&self, dir: &str) -> Option<String>;
    fn display_msg(&self, msg: &str);
    fn open_pr_web(&self, dir: &str, checks: bool);
    fn get_last_agent(&self) -> String;
    fn set_last_agent(&self, target: &str);
    fn switch_client(&self, target: &str) -> bool;
    fn switch_client_last(&self);
    fn run_lazygit(&self, dir: &str, args: &[&str]) -> i32;
    fn run_awt_popup(&self, dir: &str) -> bool;
    fn run_session_picker(&self, is_fullscreen: bool) -> bool;
}

struct RealOps;

impl CockpitOps for RealOps {
    fn is_dir(&self, path: &str) -> bool {
        !path.is_empty() && Path::new(path).is_dir()
    }

    fn tmux_pane_path(&self, session: &str) -> String {
        let target = format!("{session}:");
        tmux_out(&[
            "display-message",
            "-t",
            &target,
            "-p",
            "#{pane_current_path}",
        ])
        .unwrap_or_default()
    }

    fn current_dir(&self) -> String {
        std::env::current_dir()
            .ok()
            .map_or_else(|| ".".to_string(), |p| p.to_string_lossy().to_string())
    }

    fn keylog(&self, key: &str, state: &str, target: &str, origin: &str) {
        crate::keylog::record("cockpit", key, state, target, origin);
    }

    fn create_window(&self, raw_item: &str) {
        let _ = actions::create_window_from_raw(raw_item);
    }

    fn run_wt(&self, dir: &str, args: &[String]) -> i32 {
        let _ = std::env::set_current_dir(dir);
        crate::wt::run_cli(args)
    }

    fn git_branch(&self, dir: &str) -> String {
        Command::new("git")
            .args(["-C", dir, "rev-parse", "--abbrev-ref", "HEAD"])
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    fn git_porcelain(&self, dir: &str) -> Option<String> {
        let is_git = Command::new("git")
            .args(["-C", dir, "rev-parse", "--is-inside-work-tree"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !is_git {
            return None;
        }
        let porcelain = crate::git::safe(Path::new(dir))
            .args(["status", "--porcelain"])
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        Some(porcelain)
    }

    fn display_msg(&self, msg: &str) {
        tmux_display_msg(msg);
    }

    fn open_pr_web(&self, dir: &str, checks: bool) {
        let script = if checks {
            "cd \"$1\" 2>/dev/null && gh pr checks --web 2>/dev/null || tmux display-message 'No PR checks found for current branch'"
        } else {
            "cd \"$1\" 2>/dev/null && gh pr view --web 2>/dev/null || tmux display-message 'No pull request found for current branch'"
        };
        let _ = Command::new("sh")
            .args(["-c", script, "sh", dir])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    fn get_last_agent(&self) -> String {
        tmux_get_global_opt("@cockpit_last_agent")
    }

    fn set_last_agent(&self, target: &str) {
        tmux_set_global_opt("@cockpit_last_agent", target);
    }

    fn switch_client(&self, target: &str) -> bool {
        tmux_status(&["switch-client", "-t", target])
    }

    fn switch_client_last(&self) {
        let _ = tmux_status(&["switch-client", "-l"]);
    }

    fn run_lazygit(&self, dir: &str, args: &[&str]) -> i32 {
        let review_before = items::review_snapshot(Path::new(dir));
        let review_started = std::time::Instant::now();
        let lzg_bin = find_lazygitrs_bin();
        let rc = Command::new(&lzg_bin)
            .args(args)
            .current_dir(dir)
            .env("LAZYGITRS_COCKPIT", "1")
            .status()
            .ok()
            .and_then(|s| s.code())
            .unwrap_or(0);
        items::finish_review(Path::new(dir), &review_before, review_started.elapsed());
        rc
    }

    fn run_awt_popup(&self, dir: &str) -> bool {
        let Some(awt_popup) = crate::wt::find_user_script("awt-popup.sh") else {
            return false;
        };
        let _ = Command::new(&awt_popup).arg(dir).status();
        true
    }

    fn run_session_picker(&self, is_fullscreen: bool) -> bool {
        let Some(sp) = crate::wt::find_user_script("session-picker.sh") else {
            return false;
        };
        let mut cmd = Command::new(&sp);
        if is_fullscreen {
            cmd.arg("--fullscreen");
        }
        let _ = cmd.status();
        true
    }
}

fn resolve_target_path_with(row: &RowTarget, orig_sess: &str, ops: &dyn CockpitOps) -> String {
    let mut t_path = if ops.is_dir(&row.path) {
        row.path.clone()
    } else {
        String::new()
    };

    if t_path.is_empty() && !row.session.is_empty() {
        t_path = ops.tmux_pane_path(&row.session);
    }

    if !ops.is_dir(&t_path) && !orig_sess.is_empty() {
        t_path = ops.tmux_pane_path(orig_sess);
    }

    if !ops.is_dir(&t_path) {
        t_path = ops.current_dir();
    }

    t_path
}

fn execute(
    action: Action,
    ops: &dyn CockpitOps,
    orig_sess: &str,
    orig_win: &str,
    is_fullscreen: bool,
) -> Flow {
    let origin = format!("{orig_sess}:{orig_win}");
    match action {
        Action::Cancel => {
            ops.keylog("esc", "", "", &origin);
            Flow::Exit(0)
        }
        Action::Noop => Flow::Exit(0),
        Action::CreateWindow(raw_item) => {
            ops.create_window(&raw_item);
            Flow::Exit(0)
        }
        Action::NewWorktree(row) => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            Flow::Exit(ops.run_wt(&target_path, &["new".to_string()]))
        }
        Action::ShipWorktree(row) => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            let branch = ops.git_branch(&target_path);
            if branch.is_empty() || matches!(branch.as_str(), "main" | "master" | "HEAD") {
                ops.display_msg(&format!(
                    "Cannot ship default branch '{branch}' (not a feature worktree)"
                ));
                return Flow::Exit(0);
            }
            Flow::Exit(ops.run_wt(&target_path, &ship_cli_args(branch)))
        }
        Action::SweepWorktrees(row) => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            Flow::Exit(ops.run_wt(&target_path, &["sweep".to_string()]))
        }
        Action::ReapAgents => {
            let target_path = resolve_target_path_with(&RowTarget::default(), orig_sess, ops);
            Flow::Exit(ops.run_wt(&target_path, &["reap".to_string()]))
        }
        Action::OpenPr { row, checks } => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            ops.open_pr_web(&target_path, checks);
            Flow::Reopen(Some((row.session, row.idx)))
        }
        Action::JumpLastAgent => {
            let last_target = ops.get_last_agent();
            if origin != ":" {
                ops.set_last_agent(&origin);
            }
            // No recorded agent, or it is gone: fall back to the previous session.
            if last_target.is_empty() || !ops.switch_client(&last_target) {
                ops.switch_client_last();
            }
            Flow::Exit(0)
        }
        Action::LazyGit { row, patch_mode } => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            let Some(porcelain) = ops.git_porcelain(&target_path) else {
                ops.display_msg(&format!("Not in a git repository: {target_path}"));
                return if patch_mode {
                    Flow::Reopen(None)
                } else {
                    Flow::Exit(0)
                };
            };
            if patch_mode && porcelain.is_empty() {
                ops.display_msg(&format!(
                    "Working tree is clean: nothing to patch in {target_path}"
                ));
                return Flow::Reopen(None);
            }
            let mut lzg_args = vec!["-d", "-c", "popup"];
            if !patch_mode && porcelain.is_empty() {
                lzg_args.push("--commits");
            }
            let rc = ops.run_lazygit(&target_path, &lzg_args);
            if rc == 10 {
                Flow::Reopen(Some((row.session, row.idx)))
            } else {
                Flow::Exit(0)
            }
        }
        Action::SwitchAwt(row) => {
            let target_path = resolve_target_path_with(&row, orig_sess, ops);
            if !ops.run_awt_popup(&target_path) {
                ops.display_msg("awt-popup.sh not found (see `waymux wt popup`)");
            }
            Flow::Exit(0)
        }
        Action::SwitchSession => {
            if !ops.run_session_picker(is_fullscreen) {
                ops.display_msg("session-picker.sh not found in the awt script dirs");
            }
            Flow::Exit(0)
        }
        Action::Jump(row) => {
            let target = format!("{}:{}", row.session, row.idx);
            ops.keylog("enter", &row.state, &target, &origin);
            if !row.session.is_empty() && !row.idx.is_empty() && row.idx != "-" {
                if origin != ":" {
                    ops.set_last_agent(&origin);
                }
                let _ = ops.switch_client(&target);
            }
            Flow::Exit(0)
        }
    }
}

fn compute_start_idx(
    items_str: &str,
    saved_sess: &str,
    saved_idx: &str,
    orig_sess: &str,
    orig_win: &str,
) -> usize {
    if saved_sess.is_empty() {
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
    } else {
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

fn find_wm_bin_in(home: Option<&Path>) -> String {
    if let Some(home) = home {
        for name in ["wm", "mm"] {
            let candidate = home.join(".local/bin").join(name);
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    "wm".to_string()
}

fn find_wm_bin() -> String {
    let home = std::env::var_os("HOME");
    find_wm_bin_in(home.as_deref().map(Path::new))
}

fn find_lazygitrs_bin_in(home: Option<&Path>) -> String {
    if let Some(home) = home {
        for rel in [".local/bin/lazygitrs", ".cargo/bin/lazygitrs"] {
            let candidate = home.join(rel);
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    "lazygitrs".to_string()
}

fn find_lazygitrs_bin() -> String {
    let home = std::env::var_os("HOME");
    find_lazygitrs_bin_in(home.as_deref().map(Path::new))
}

const COCKPIT_BADGE: &str = " 󱂬 ";

fn build_popup_args(
    orig_sess: &str,
    orig_win: &str,
    exe_str: &str,
    border_color: &str,
    text_col: &str,
    gradient: &str,
) -> Vec<String> {
    let popup_cmd = format!(
        "TMUX_POPUP=1 TMUX_ORIGIN_SESSION={} TMUX_ORIGIN_WINDOW={} {} cockpit",
        shell_quote(orig_sess),
        shell_quote(orig_win),
        shell_quote(exe_str)
    );
    let mut args = vec![
        "display-popup".to_string(),
        "-S".to_string(),
        format!("fg={border_color}"),
        "-s".to_string(),
        format!("fg={text_col}"),
        "-b".to_string(),
        "rounded".to_string(),
        "-T".to_string(),
        COCKPIT_BADGE.to_string(),
    ];
    let trimmed = gradient.trim();
    if !trimmed.is_empty() {
        args.extend([
            "-B".to_string(),
            "-e".to_string(),
            format!("WM_BORDER_GRADIENT={trimmed}"),
            "-e".to_string(),
            format!("WM_BORDER_TITLE={COCKPIT_BADGE}"),
        ]);
    }
    args.extend([
        "-w".to_string(),
        "85%".to_string(),
        "-h".to_string(),
        "75%".to_string(),
        "-E".to_string(),
        popup_cmd,
    ]);
    args
}

fn resolve_color_spec(
    env_spec: Option<&str>,
    seg_bg: &str,
    fg_opt: &str,
    accent_opt: &str,
) -> String {
    if let Some(spec) = env_spec.filter(|s| !s.is_empty()) {
        return spec.to_string();
    }
    if !seg_bg.is_empty() && !fg_opt.is_empty() && !accent_opt.is_empty() {
        format!(
            "list-border:{seg_bg},group-header:{fg_opt},selected-prefix:{accent_opt},selected-fg:{fg_opt}"
        )
    } else {
        "list-border:239,group-header:245,selected-prefix:214,selected-fg:223".to_string()
    }
}

fn resolve_spinner_color(env_color: Option<&str>, accent_opt: &str) -> String {
    env_color
        .filter(|s| !s.is_empty())
        .or_else(|| (!accent_opt.is_empty()).then_some(accent_opt))
        .unwrap_or("214")
        .to_string()
}

pub fn run_cockpit(args: &[String]) -> i32 {
    let is_fullscreen = args.iter().any(|a| a == "--fullscreen");
    let exe_str = std::env::current_exe()
        .ok()
        .map_or_else(|| "waymux".to_string(), |p| p.to_string_lossy().to_string());

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
        let gradient = tmux_get_global_opt("@border_gradient");
        let popup_args = build_popup_args(
            &orig_sess,
            &orig_win,
            &exe_str,
            &theme.orange,
            &text_col,
            &gradient,
        );
        let refs: Vec<&str> = popup_args.iter().map(String::as_str).collect();
        let _ = tmux_status(&refs);
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

    let env_color_spec = std::env::var("TMUX_COLOR_SPEC").ok();
    let color_spec = resolve_color_spec(env_color_spec.as_deref(), &seg_bg, &fg_opt, &accent_opt);
    let env_spinner_color = std::env::var("TMUX_SPINNER_COLOR").ok();
    let spinner_color = resolve_spinner_color(env_spinner_color.as_deref(), &accent_opt);

    let origin = format!("{orig_sess}:{orig_win}");
    let ops = RealOps;
    loop {
        let items_str = items::generate_items(false, &orig_sess, &orig_win);
        let start_idx =
            compute_start_idx(&items_str, &saved_sess, &saved_idx, &orig_sess, &orig_win);

        let Ok(mut child) = Command::new(&wm_bin)
            .args([
                "--config",
                &toml_path.to_string_lossy(),
                &format!("results.spinner={spinner_name}"),
                "--pos",
                &start_idx.to_string(),
                "--color",
                &format!("spinner:{spinner_color}"),
                "--color",
                &color_spec,
                "--group-prefix",
                "#",
            ])
            .env(crate::keylog::ORIGIN_ENV, &origin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        else {
            return 1;
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(items_str.as_bytes());
        }

        let Ok(output) = child.wait_with_output() else {
            return 0;
        };

        let chosen = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();

        match execute(
            parse_action(&chosen),
            &ops,
            &orig_sess,
            &orig_win,
            is_fullscreen,
        ) {
            Flow::Exit(code) => return code,
            Flow::Reopen(Some((sess, idx))) => {
                saved_sess = sess;
                saved_idx = idx;
            }
            Flow::Reopen(None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn row(idx: &str, sess: &str, state: &str) -> String {
        format!("t\t{idx}\tname\t{sess}\t%1\t/repo\t{state}\t-\t-\t-\t-\t-")
    }

    #[derive(Default)]
    struct FakeOps {
        dirs: Vec<String>,
        pane_paths: HashMap<String, String>,
        cwd: String,
        branch: String,
        porcelain: Option<String>,
        last_agent: RefCell<String>,
        switch_ok: bool,
        lazygit_rc: i32,
        awt_popup_exists: bool,
        session_picker_exists: bool,
        wt_rc: i32,
        calls: RefCell<Vec<String>>,
    }

    impl FakeOps {
        fn new() -> Self {
            Self {
                dirs: vec!["/repo".to_string(), "/fallback".to_string()],
                cwd: "/cwd".to_string(),
                branch: "feat/x".to_string(),
                porcelain: Some(" M a.rs".to_string()),
                switch_ok: true,
                awt_popup_exists: true,
                session_picker_exists: true,
                ..Self::default()
            }
        }

        fn log(&self, entry: impl Into<String>) {
            self.calls.borrow_mut().push(entry.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl CockpitOps for FakeOps {
        fn is_dir(&self, path: &str) -> bool {
            self.dirs.iter().any(|d| d == path)
        }

        fn tmux_pane_path(&self, session: &str) -> String {
            self.pane_paths.get(session).cloned().unwrap_or_default()
        }

        fn current_dir(&self) -> String {
            self.cwd.clone()
        }

        fn keylog(&self, key: &str, state: &str, target: &str, origin: &str) {
            self.log(format!("keylog:{key}:{state}:{target}:{origin}"));
        }

        fn create_window(&self, raw_item: &str) {
            self.log(format!("create_window:{raw_item}"));
        }

        fn run_wt(&self, dir: &str, args: &[String]) -> i32 {
            self.log(format!("wt:{dir}:{}", args.join(" ")));
            self.wt_rc
        }

        fn git_branch(&self, _dir: &str) -> String {
            self.branch.clone()
        }

        fn git_porcelain(&self, _dir: &str) -> Option<String> {
            self.porcelain.clone()
        }

        fn display_msg(&self, msg: &str) {
            self.log(format!("msg:{msg}"));
        }

        fn open_pr_web(&self, dir: &str, checks: bool) {
            self.log(format!("pr:{dir}:checks={checks}"));
        }

        fn get_last_agent(&self) -> String {
            self.last_agent.borrow().clone()
        }

        fn set_last_agent(&self, target: &str) {
            *self.last_agent.borrow_mut() = target.to_string();
            self.log(format!("set_last:{target}"));
        }

        fn switch_client(&self, target: &str) -> bool {
            self.log(format!("switch:{target}"));
            self.switch_ok
        }

        fn switch_client_last(&self) {
            self.log("switch_last");
        }

        fn run_lazygit(&self, dir: &str, args: &[&str]) -> i32 {
            self.log(format!("lazygit:{dir}:{}", args.join(" ")));
            self.lazygit_rc
        }

        fn run_awt_popup(&self, dir: &str) -> bool {
            self.log(format!("awt_popup:{dir}"));
            self.awt_popup_exists
        }

        fn run_session_picker(&self, is_fullscreen: bool) -> bool {
            self.log(format!("session_picker:fs={is_fullscreen}"));
            self.session_picker_exists
        }
    }

    #[test]
    fn test_parse_action_covers_every_prefix_and_row_shape() {
        let r = row("2", "api", "permission");
        let expected_row = RowTarget {
            raw: r.clone(),
            session: "api".to_string(),
            idx: "2".to_string(),
            path: "/repo".to_string(),
            state: "permission".to_string(),
        };
        assert_eq!(parse_action(""), Action::Cancel);
        assert_eq!(parse_action("  (no active AI agents)  "), Action::Noop);
        assert_eq!(
            parse_action("__CREATE_WINDOW__#  api"),
            Action::CreateWindow("#  api".to_string())
        );
        assert_eq!(
            parse_action(&format!("__NEW_WORKTREE__{r}")),
            Action::NewWorktree(expected_row.clone())
        );
        assert_eq!(
            parse_action(&format!("__SHIP_WORKTREE__{r}")),
            Action::ShipWorktree(expected_row.clone())
        );
        assert_eq!(
            parse_action(&format!("__SWEEP_WORKTREES__{r}")),
            Action::SweepWorktrees(expected_row.clone())
        );
        assert_eq!(parse_action("__REAP_AGENTS__"), Action::ReapAgents);
        assert_eq!(
            parse_action(&format!("__OPEN_PR__{r}")),
            Action::OpenPr {
                row: expected_row.clone(),
                checks: false
            }
        );
        assert_eq!(
            parse_action(&format!("__OPEN_PR_CHECKS__{r}")),
            Action::OpenPr {
                row: expected_row.clone(),
                checks: true
            }
        );
        assert_eq!(parse_action("__JUMP_LAST_AGENT__"), Action::JumpLastAgent);
        assert_eq!(
            parse_action(&format!("__LAZYGIT__{r}")),
            Action::LazyGit {
                row: expected_row.clone(),
                patch_mode: false
            }
        );
        assert_eq!(
            parse_action(&format!("__PATCH_MODE__{r}")),
            Action::LazyGit {
                row: expected_row.clone(),
                patch_mode: true
            }
        );
        assert_eq!(
            parse_action("__SWITCH_AWT__#  api  2 windows"),
            Action::SwitchAwt(RowTarget {
                raw: "#  api  2 windows".to_string(),
                session: "api".to_string(),
                ..RowTarget::default()
            })
        );
        assert_eq!(parse_action("__SWITCH_SESSION__"), Action::SwitchSession);
        assert_eq!(parse_action(&r), Action::Jump(expected_row));
    }

    #[test]
    fn test_resolve_target_path_falls_back_through_session_origin_and_cwd() {
        let mut ops = FakeOps::new();
        ops.pane_paths
            .insert("sess".to_string(), "/fallback".to_string());
        ops.pane_paths
            .insert("orig".to_string(), "/repo".to_string());

        // Row path is a valid dir -> used directly.
        let r = RowTarget::parse(&row("1", "sess", "idle"));
        assert_eq!(resolve_target_path_with(&r, "orig", &ops), "/repo");

        // Group header has only a session -> queries that session's pane path.
        let header = RowTarget::parse("#  sess");
        assert_eq!(resolve_target_path_with(&header, "orig", &ops), "/fallback");

        // Unknown session -> falls back to origin session's pane path.
        let unknown = RowTarget::parse("#  missing");
        assert_eq!(resolve_target_path_with(&unknown, "orig", &ops), "/repo");

        // Neither session resolves to a dir -> falls back to current_dir.
        assert_eq!(resolve_target_path_with(&unknown, "missing", &ops), "/cwd");
    }

    #[test]
    fn test_execute_cancel_noop_create_window_and_jump() {
        let ops = FakeOps::new();
        assert_eq!(
            execute(Action::Cancel, &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::Noop, &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(
                Action::CreateWindow("#  api".to_string()),
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Exit(0)
        );
        let r = RowTarget::parse(&row("2", "api", "permission"));
        assert_eq!(
            execute(Action::Jump(r), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        // Jumping to a header row (idx empty) records keylog without switching client.
        let header = RowTarget::parse("#  api");
        assert_eq!(
            execute(Action::Jump(header), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            ops.calls(),
            vec![
                "keylog:esc:::orig:0",
                "create_window:#  api",
                "keylog:enter:permission:api:2:orig:0",
                "set_last:orig:0",
                "switch:api:2",
                "keylog:enter::api::orig:0",
            ]
        );
    }

    #[test]
    fn test_execute_worktree_actions_and_ship_guard() {
        let mut ops = FakeOps::new();
        let r = RowTarget::parse(&row("1", "api", "idle"));

        assert_eq!(
            execute(Action::NewWorktree(r.clone()), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::SweepWorktrees(r.clone()), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::ReapAgents, &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::ShipWorktree(r.clone()), &ops, "orig", "0", false),
            Flow::Exit(0)
        );

        // Shipping main/master/HEAD is refused with a tmux message.
        for protected in ["main", "master", "HEAD", ""] {
            ops.branch = protected.to_string();
            assert_eq!(
                execute(Action::ShipWorktree(r.clone()), &ops, "orig", "0", false),
                Flow::Exit(0)
            );
        }

        let calls = ops.calls();
        assert!(calls.contains(&"wt:/repo:new".to_string()));
        assert!(calls.contains(&"wt:/repo:sweep".to_string()));
        assert!(calls.contains(&"wt:/cwd:reap".to_string()));
        assert!(calls.contains(&"wt:/repo:ship feat/x --confirm".to_string()));
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("msg:Cannot ship default branch 'main'"))
        );
    }

    #[test]
    fn test_execute_open_pr_and_jump_last_agent() {
        let mut ops = FakeOps::new();
        let r = RowTarget::parse(&row("3", "api", "review"));

        assert_eq!(
            execute(
                Action::OpenPr {
                    row: r.clone(),
                    checks: false
                },
                &ops,
                "orig",
                "1",
                false
            ),
            Flow::Reopen(Some(("api".to_string(), "3".to_string())))
        );
        assert_eq!(
            execute(
                Action::OpenPr {
                    row: r,
                    checks: true
                },
                &ops,
                "orig",
                "1",
                false
            ),
            Flow::Reopen(Some(("api".to_string(), "3".to_string())))
        );

        // No recorded last agent -> falls back to switch-client -l.
        assert_eq!(
            execute(Action::JumpLastAgent, &ops, "orig", "1", false),
            Flow::Exit(0)
        );
        // Now `@cockpit_last_agent` is set to `orig:1`; jumping switches to it, unless switch fails.
        assert_eq!(
            execute(Action::JumpLastAgent, &ops, "api", "3", false),
            Flow::Exit(0)
        );
        ops.switch_ok = false;
        assert_eq!(
            execute(Action::JumpLastAgent, &ops, "orig", "1", false),
            Flow::Exit(0)
        );

        assert_eq!(
            ops.calls(),
            vec![
                "pr:/repo:checks=false",
                "pr:/repo:checks=true",
                "set_last:orig:1",
                "switch_last",
                "set_last:api:3",
                "switch:orig:1",
                "set_last:orig:1",
                "switch:api:3",
                "switch_last",
            ]
        );
    }

    #[test]
    fn test_execute_lazygit_and_patch_mode_branches() {
        let mut ops = FakeOps::new();
        let r = RowTarget::parse(&row("1", "api", "review"));

        // Dirty tree: opens lazygit in files view; exit 0 closes Cockpit, exit 10 reopens.
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r.clone(),
                    patch_mode: false
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Exit(0)
        );
        ops.lazygit_rc = 10;
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r.clone(),
                    patch_mode: false
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Reopen(Some(("api".to_string(), "1".to_string())))
        );

        // Clean tree: regular lazygit passes --commits; patch mode warns and reopens.
        ops.porcelain = Some(String::new());
        ops.lazygit_rc = 0;
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r.clone(),
                    patch_mode: false
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r.clone(),
                    patch_mode: true
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Reopen(None)
        );

        // Outside a git repo: regular lazygit exits, patch mode reopens.
        ops.porcelain = None;
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r.clone(),
                    patch_mode: false
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(
                Action::LazyGit {
                    row: r,
                    patch_mode: true
                },
                &ops,
                "orig",
                "0",
                false
            ),
            Flow::Reopen(None)
        );

        let calls = ops.calls();
        assert!(calls.contains(&"lazygit:/repo:-d -c popup".to_string()));
        assert!(calls.contains(&"lazygit:/repo:-d -c popup --commits".to_string()));
        assert!(
            calls.contains(&"msg:Working tree is clean: nothing to patch in /repo".to_string())
        );
        assert!(calls.contains(&"msg:Not in a git repository: /repo".to_string()));
    }

    #[test]
    fn test_execute_switch_awt_and_switch_session() {
        let mut ops = FakeOps::new();
        let r = RowTarget::parse(&row("1", "api", "idle"));

        assert_eq!(
            execute(Action::SwitchAwt(r.clone()), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::SwitchSession, &ops, "orig", "0", true),
            Flow::Exit(0)
        );

        ops.awt_popup_exists = false;
        ops.session_picker_exists = false;
        assert_eq!(
            execute(Action::SwitchAwt(r), &ops, "orig", "0", false),
            Flow::Exit(0)
        );
        assert_eq!(
            execute(Action::SwitchSession, &ops, "orig", "0", false),
            Flow::Exit(0)
        );

        assert_eq!(
            ops.calls(),
            vec![
                "awt_popup:/repo",
                "session_picker:fs=true",
                "awt_popup:/repo",
                "msg:awt-popup.sh not found (see `waymux wt popup`)",
                "session_picker:fs=false",
                "msg:session-picker.sh not found in the awt script dirs",
            ]
        );
    }

    #[test]
    fn test_popup_args_colors_and_binary_lookup() {
        let plain = build_popup_args("main", "1", "/bin/waymux", "#fab387", "default", "");
        assert!(!plain.contains(&"-B".to_string()));
        assert!(plain.contains(&"85%".to_string()));

        let grad = build_popup_args("main", "1", "/bin/waymux", "#fab387", "default", " aurora ");
        assert!(grad.contains(&"-B".to_string()));
        assert!(grad.contains(&"WM_BORDER_GRADIENT=aurora".to_string()));

        assert_eq!(
            resolve_color_spec(Some("custom:1"), "a", "b", "c"),
            "custom:1"
        );
        assert_eq!(
            resolve_color_spec(None, "236", "252", "214"),
            "list-border:236,group-header:252,selected-prefix:214,selected-fg:252"
        );
        assert_eq!(
            resolve_color_spec(None, "", "", ""),
            "list-border:239,group-header:245,selected-prefix:214,selected-fg:223"
        );
        assert_eq!(resolve_spinner_color(Some("120"), "214"), "120");
        assert_eq!(resolve_spinner_color(None, "215"), "215");
        assert_eq!(resolve_spinner_color(None, ""), "214");

        let dir = std::env::temp_dir().join(format!("waymux-cockpit-bins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".local/bin")).unwrap();
        std::fs::create_dir_all(dir.join(".cargo/bin")).unwrap();
        assert_eq!(find_wm_bin_in(Some(&dir)), "wm");
        assert_eq!(find_lazygitrs_bin_in(Some(&dir)), "lazygitrs");

        std::fs::write(dir.join(".local/bin/mm"), "").unwrap();
        std::fs::write(dir.join(".cargo/bin/lazygitrs"), "").unwrap();
        assert!(find_wm_bin_in(Some(&dir)).ends_with(".local/bin/mm"));
        assert!(find_lazygitrs_bin_in(Some(&dir)).ends_with(".cargo/bin/lazygitrs"));

        std::fs::write(dir.join(".local/bin/wm"), "").unwrap();
        std::fs::write(dir.join(".local/bin/lazygitrs"), "").unwrap();
        assert!(find_wm_bin_in(Some(&dir)).ends_with(".local/bin/wm"));
        assert!(find_lazygitrs_bin_in(Some(&dir)).ends_with(".local/bin/lazygitrs"));
        let _ = std::fs::remove_dir_all(&dir);
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
        // Saved session without window index selects the first row of that session.
        assert_eq!(
            compute_start_idx(&items, "dotfiles", "-", "dotfiles", "1"),
            1
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
