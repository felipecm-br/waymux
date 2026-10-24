use crate::sys::shell_quote;
use crate::tmux_cmd::{
    tmux_display_msg, tmux_display_msg_timed, tmux_get_global_opt, tmux_out, tmux_set_global_opt,
    tmux_status, tmux_unset_global_opt,
};
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn is_valid_pane_id(pid: &str) -> bool {
    pid.starts_with('%') && pid.len() > 1 && pid[1..].chars().all(|c| c.is_ascii_digit())
}

pub fn can_confirm_state(state: &str) -> bool {
    matches!(state, "permission" | "question" | "awaiting_input")
}

pub fn run_agent_action(args: &[String]) -> i32 {
    let action = args.first().map(|s| s.as_str()).unwrap_or("");
    let state = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let pid = args.get(2).map(|s| s.as_str()).unwrap_or("");

    if !is_valid_pane_id(pid) {
        return 0;
    }

    match action {
        "approve" => {
            if can_confirm_state(state) {
                let _ = tmux_status(&["send-keys", "-t", pid, "y", "Enter"]);
            } else {
                let st = if state.is_empty() { "normal" } else { state };
                tmux_display_msg(&format!(
                    "Action ignored: Pane {} is not awaiting confirmation (state: {})",
                    pid, st
                ));
            }
            0
        }
        "deny" => {
            if can_confirm_state(state) {
                let _ = tmux_status(&["send-keys", "-t", pid, "n", "Enter"]);
            } else {
                let st = if state.is_empty() { "normal" } else { state };
                tmux_display_msg(&format!(
                    "Action ignored: Pane {} is not awaiting confirmation (state: {})",
                    pid, st
                ));
            }
            0
        }
        "prompt" => {
            if args.len() > 3 {
                let prompt_text = args[3..].join(" ");
                if !prompt_text.is_empty() {
                    let _ = tmux_status(&["send-keys", "-l", "-t", pid, "--", &prompt_text]);
                    let _ = tmux_status(&["send-keys", "-t", pid, "Enter"]);
                }
            }
            0
        }
        _ => {
            eprintln!("Usage: waymux agent-action {{approve|deny|prompt}} <state> <pid> [args...]");
            1
        }
    }
}

pub fn create_window_from_raw(raw_input: &str) -> Option<String> {
    let mut session = String::new();
    let mut row_path = String::new();

    if !raw_input.trim().is_empty()
        && let Some(first_line) = raw_input.lines().next()
    {
        if let Some(stripped) = first_line.strip_prefix('#') {
            let header_sess = stripped.trim();
            if !header_sess.is_empty() && tmux_status(&["has-session", "-t", header_sess]) {
                session = header_sess.to_string();
            }
        } else {
            let cols: Vec<&str> = first_line.split('\t').collect();
            if let Some(s) = cols.get(3) {
                session = (*s).to_string();
            }
            if let Some(p) = cols.get(5)
                && !p.is_empty()
                && Path::new(p).is_dir()
            {
                row_path = (*p).to_string();
            }
        }
    }

    if session.is_empty() || !tmux_status(&["has-session", "-t", &session]) {
        session = std::env::var("TMUX_ORIGIN_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| tmux_out(&["display-message", "-p", "#S"]))
            .unwrap_or_default();
    }

    if session.is_empty() {
        return None;
    }

    if row_path.is_empty() {
        let target_sess = format!("{}:", session);
        if let Some(sdir) = tmux_out(&[
            "display-message",
            "-t",
            &target_sess,
            "-p",
            "#{pane_current_path}",
        ]) && Path::new(&sdir).is_dir()
        {
            row_path = sdir;
        }
    }

    let target_sess = format!("{}:", session);
    let new_target = if !row_path.is_empty() && Path::new(&row_path).is_dir() {
        tmux_out(&[
            "new-window",
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
            "-c",
            &row_path,
            "-t",
            &target_sess,
        ])
        .or_else(|| {
            tmux_out(&[
                "new-window",
                "-P",
                "-F",
                "#{session_name}:#{window_index}",
                "-t",
                &target_sess,
            ])
        })
    } else {
        tmux_out(&[
            "new-window",
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
            "-t",
            &target_sess,
        ])
    };

    if let Some(ref nt) = new_target {
        let _ = tmux_status(&["switch-client", "-t", nt]);
    }
    new_target
}

pub fn run_window_create() -> i32 {
    let mut raw = String::new();
    let _ = io::stdin().read_to_string(&mut raw);
    if let Some(target) = create_window_from_raw(&raw) {
        println!("{}", target);
    }
    0
}

/// Tmux option holding the armed kill target as `<target>|<epoch ms>`.
const KILL_ARM_OPT: &str = "@cockpit_kill_armed";
/// Window in which a second press confirms killing a target with in-flight agent work.
pub const KILL_CONFIRM_MS: u64 = 2000;

/// States that mean in-flight agent work; mirrors close-window.sh (everything but idle/done).
pub fn is_active_agent_state(state: &str) -> bool {
    !matches!(
        state,
        "" | "idle" | "done" | "closed" | "inactive" | "completed" | "normal"
    )
}

/// Live state of one pane: the daemon wins over the tmux option, and an agent CLI running
/// without lifecycle hooks counts as "running", like close-window.sh.
pub fn pane_agent_state(daemon_state: Option<&str>, raw_state: &str, current_cmd: &str) -> String {
    let st = daemon_state
        .filter(|s| !s.is_empty())
        .unwrap_or(raw_state)
        .trim();
    if st.is_empty() && crate::cockpit::items::is_agent_binary_name(current_cmd) {
        "running".to_string()
    } else {
        st.to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum KillDecision {
    Kill,
    Arm,
}

/// Idle targets die on the first press (Prefix+u restores them). A target with in-flight
/// agent work needs a second press on the same target within `KILL_CONFIRM_MS`, because
/// undo restores the window and conversation but not the interrupted run.
pub fn kill_decision(
    has_active_agent: bool,
    armed: &str,
    target: &str,
    now_ms: u64,
) -> KillDecision {
    if !has_active_agent {
        return KillDecision::Kill;
    }
    match armed.rsplit_once('|') {
        Some((t, ts)) if t == target => match ts.parse::<u64>() {
            Ok(ts) if now_ms.saturating_sub(ts) <= KILL_CONFIRM_MS => KillDecision::Kill,
            _ => KillDecision::Arm,
        },
        _ => KillDecision::Arm,
    }
}

/// Active agent states in a window (`sess:idx`) or, with `whole_session`, in a whole session.
fn active_agent_states(target: &str, whole_session: bool) -> Vec<String> {
    let daemon = crate::rpc_client::fetch_agent_states();
    let fmt = "#{pane_id}\t#{pane_current_command}\t#{?@ai_agent_state_raw,#{@ai_agent_state_raw},#{@w:@ai_agent_state_raw}}";
    let mut args = vec!["list-panes"];
    if whole_session {
        args.push("-s");
    }
    args.extend(["-t", target, "-F", fmt]);
    tmux_out(&args)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let pane = cols.next()?;
            let cmd = cols.next().unwrap_or("");
            let raw = cols.next().unwrap_or("");
            let st = pane_agent_state(daemon.get(pane).map(|e| e.state.as_str()), raw, cmd);
            is_active_agent_state(&st).then_some(st)
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Returns true when the kill may proceed; otherwise arms `key` and tells the user why.
fn confirm_kill(key: &str, label: &str, states: &[String]) -> bool {
    let now = now_ms();
    let armed = tmux_get_global_opt(KILL_ARM_OPT);
    match kill_decision(!states.is_empty(), &armed, key, now) {
        KillDecision::Kill => {
            if !armed.is_empty() {
                tmux_unset_global_opt(KILL_ARM_OPT);
            }
            true
        }
        KillDecision::Arm => {
            tmux_set_global_opt(KILL_ARM_OPT, &format!("{}|{}", key, now));
            tmux_display_msg_timed(
                KILL_CONFIRM_MS as u32,
                &format!(
                    " 󰚌 {}: agent {} │ press again to kill",
                    label,
                    states.first().map(String::as_str).unwrap_or("active")
                ),
            );
            false
        }
    }
}

pub fn run_window_delete() -> i32 {
    let mut raw = String::new();
    let _ = io::stdin().read_to_string(&mut raw);
    if raw.trim().is_empty() {
        return 0;
    }

    let first_line = raw.lines().next().unwrap_or("");
    if let Some(stripped) = first_line.strip_prefix('#') {
        let sess_name = stripped.trim();
        if sess_name.is_empty() {
            return 0;
        }

        let target_sess = format!("{}:", sess_name);
        let wt_path = tmux_out(&[
            "display-message",
            "-t",
            &target_sess,
            "-p",
            "#{pane_current_path}",
        ])
        .unwrap_or_default();

        if !wt_path.is_empty() && Path::new(&wt_path).is_dir() {
            let git_common = Command::new("git")
                .args(["-C", &wt_path, "rev-parse", "--git-common-dir"])
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let git_dir = Command::new("git")
                .args(["-C", &wt_path, "rev-parse", "--git-dir"])
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            if !git_common.is_empty() && !git_dir.is_empty() && git_common != git_dir {
                let branch = Command::new("git")
                    .args(["-C", &wt_path, "rev-parse", "--abbrev-ref", "HEAD"])
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();

                if !branch.is_empty()
                    && branch != "main"
                    && branch != "master"
                    && let Some(awt_del) = crate::wt::find_waymaker_script("awt-delete.sh")
                {
                    let awt_del = awt_del.to_string_lossy().to_string();
                    if std::env::var("TMUX").is_ok() {
                        let cmd_str = format!(
                            "{} {} {} {} --confirm",
                            shell_quote(&awt_del),
                            shell_quote(sess_name),
                            shell_quote(&wt_path),
                            shell_quote(&branch)
                        );
                        let _ = tmux_status(&[
                            "display-popup",
                            "-b",
                            "rounded",
                            "-w",
                            "60",
                            "-h",
                            "12",
                            "-E",
                            &cmd_str,
                        ]);
                    } else {
                        let _ = Command::new(&awt_del)
                            .args([sess_name, &wt_path, &branch, "--confirm"])
                            .status();
                    }
                    return 0;
                }
            }
        }

        if !confirm_kill(
            &target_sess,
            sess_name,
            &active_agent_states(&target_sess, true),
        ) {
            return 0;
        }

        let cur_sess = tmux_out(&["display-message", "-p", "#{session_name}"]).unwrap_or_default();
        if cur_sess == sess_name && !tmux_status(&["switch-client", "-l"]) {
            let _ = tmux_status(&["switch-client", "-n"]);
        }
        let _ = tmux_status(&["kill-session", "-t", sess_name]);
        return 0;
    }

    let mut targets = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if let (Some(sess), Some(idx)) = (cols.get(3), cols.get(1))
            && !sess.is_empty()
            && !idx.is_empty()
            && *idx != "-"
        {
            targets.push(((*sess).to_string(), (*idx).to_string()));
        }
    }

    if targets.is_empty() {
        return 0;
    }

    // Optional user script that records a window before it is killed (e.g. to reopen it).
    let record_script = crate::wt::find_user_script("record-window-state.sh");

    for (sess, idx) in targets {
        let target = format!("{}:{}", sess, idx);
        let label = tmux_out(&[
            "display-message",
            "-p",
            "-t",
            &target,
            "#{session_name} › #{window_name}",
        ])
        .unwrap_or_else(|| target.clone());
        if !confirm_kill(&target, &label, &active_agent_states(&target, false)) {
            continue;
        }

        let is_active = tmux_out(&["display-message", "-p", "-t", &target, "#{window_active}"])
            .unwrap_or_default();
        let win_count = tmux_out(&["list-windows", "-t", &sess])
            .map(|s| s.lines().count())
            .unwrap_or(0);

        if is_active == "1"
            && win_count > 1
            && !tmux_status(&["select-window", "-t", &sess, "-l"])
            && !tmux_status(&["previous-window", "-t", &sess])
        {
            let _ = tmux_status(&["next-window", "-t", &sess]);
        }

        if let Some(record_script) = &record_script {
            let _ = Command::new(record_script)
                .args(["--push", &target])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = tmux_status(&["kill-window", "-t", &target]);
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_pane_id() {
        assert!(is_valid_pane_id("%0"));
        assert!(is_valid_pane_id("%123"));
        assert!(!is_valid_pane_id(""));
        assert!(!is_valid_pane_id("-"));
        assert!(!is_valid_pane_id("%"));
        assert!(!is_valid_pane_id("%12a"));
    }

    #[test]
    fn test_can_confirm_state() {
        assert!(can_confirm_state("permission"));
        assert!(can_confirm_state("question"));
        assert!(can_confirm_state("awaiting_input"));
        assert!(!can_confirm_state("working"));
        assert!(!can_confirm_state("idle"));
        assert!(!can_confirm_state("normal"));
    }

    #[test]
    fn test_active_agent_states_match_close_window() {
        for st in [
            "busy",
            "working",
            "question",
            "permission",
            "stalled",
            "error",
            "running",
        ] {
            assert!(is_active_agent_state(st), "{st} should guard");
        }
        for st in [
            "",
            "idle",
            "done",
            "closed",
            "inactive",
            "completed",
            "normal",
        ] {
            assert!(!is_active_agent_state(st), "{st} should not guard");
        }
    }

    #[test]
    fn test_pane_agent_state_prefers_daemon_and_detects_hookless_agents() {
        assert_eq!(
            pane_agent_state(Some("permission"), "idle", "zsh"),
            "permission"
        );
        assert_eq!(pane_agent_state(Some(""), "busy", "zsh"), "busy");
        assert_eq!(pane_agent_state(None, "", "claude"), "running");
        assert_eq!(pane_agent_state(None, "", "zsh"), "");
    }

    #[test]
    fn test_kill_decision_guards_only_active_agents() {
        let now = 1_000_000;
        assert_eq!(kill_decision(false, "", "s:1", now), KillDecision::Kill);
        assert_eq!(kill_decision(true, "", "s:1", now), KillDecision::Arm);
        // Second press on the same target inside the window kills.
        assert_eq!(
            kill_decision(true, "s:1|999000", "s:1", now),
            KillDecision::Kill
        );
        // Too late, another target, or garbage re-arms instead.
        assert_eq!(
            kill_decision(true, "s:1|990000", "s:1", now),
            KillDecision::Arm
        );
        assert_eq!(
            kill_decision(true, "s:2|999000", "s:1", now),
            KillDecision::Arm
        );
        assert_eq!(kill_decision(true, "s:1|x", "s:1", now), KillDecision::Arm);
        // Session keys (`sess:`) never confirm a window key (`sess:1`).
        assert_eq!(
            kill_decision(true, "s:|999000", "s:1", now),
            KillDecision::Arm
        );
    }
}
