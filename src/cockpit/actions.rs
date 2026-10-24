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

/// How long a prompt must have been on screen before `y`/`n` answer it, so a key pressed
/// while the Cockpit list moves under the cursor never answers a prompt just arrived.
pub const DWELL_MS: u64 = 400;

/// Whether `y`/`n` may answer the pane now. The daemon's live state wins over the state the
/// Cockpit row was drawn with (it can be seconds old); without a daemon entry the row's state
/// is all there is. `changed_at` 0 means the daemon does not know when the state began.
pub fn confirm_check(
    row_state: &str,
    live: Option<(&str, u64)>,
    now_ms: u64,
) -> Result<(), String> {
    let (state, changed_at) = live.unwrap_or((row_state, 0));
    if !can_confirm_state(state) {
        let st = if state.is_empty() { "normal" } else { state };
        return Err(format!("is not awaiting confirmation (state: {st})"));
    }
    if changed_at > 0 && now_ms.saturating_sub(changed_at) < DWELL_MS {
        return Err("prompt just appeared: read it, then press again".into());
    }
    Ok(())
}

/// Keys that answer a permission or question prompt. Verified on Claude Code 2.1.310: `y`
/// and `n` are not hotkeys there and the highlighted option is "1. Yes", so the old `n` +
/// `Enter` *approved* the call; `1` approves at once and `Esc` denies. `Esc` also cancels in
/// the other harnesses' TUIs; their approve keeps `y` + `Enter` until verified.
pub fn answer_keys(harness: &str, approve: bool) -> &'static [&'static str] {
    match (harness, approve) {
        (_, false) => &["Escape"],
        ("claude", true) => &["1"],
        (_, true) => &["y", "Enter"],
    }
}

/// How long the first `y` on a risky call stays armed.
pub const RISK_ACK_MS: u64 = 3000;

/// Second press of `y` on a risky call: confirmed only for the same prompt (`prompt_key`,
/// the time the pane entered its state) within [`RISK_ACK_MS`] of the first press.
pub fn risk_ack_confirms(armed: Option<(u64, u64)>, prompt_key: u64, now_ms: u64) -> bool {
    armed.is_some_and(|(key, expires)| key == prompt_key && now_ms <= expires)
}

fn risk_ack_file(pid: &str) -> Option<std::path::PathBuf> {
    let safe: String = pid.chars().filter(char::is_ascii_digit).collect();
    crate::presets::private_runtime_dir().map(|d| d.join(format!("risk-ack-{safe}")))
}

fn read_risk_ack(path: &Path) -> Option<(u64, u64)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut parts = raw.split_whitespace().map(str::parse::<u64>);
    Some((parts.next()?.ok()?, parts.next()?.ok()?))
}

/// The shell command behind the pane's prompt: the hook's raw memo, else the part of the
/// daemon's one-line reason after `Tool: `.
pub fn pending_command(pid: &str, reason: Option<&str>) -> Option<String> {
    crate::hook::recall_pane_command(pid).or_else(|| {
        reason
            .and_then(|r| r.split_once(": "))
            .map(|(_, cmd)| cmd.to_string())
    })
}

fn one_line_excerpt(cmd: &str, max: usize) -> String {
    let flat = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// The rules `y` acknowledges when it may answer now (empty for a call no rule matches), or
/// `None` when this press only armed a risky call (or it cannot be confirmed here). A risky
/// call needs a second `y` within 3 s for the same prompt (`prompt_key`).
fn risk_gate(
    pid: &str,
    cmd: Option<&str>,
    prompt_key: u64,
    now_ms: u64,
) -> Option<Vec<&'static str>> {
    let Some(cmd) = cmd else {
        return Some(Vec::new());
    };
    let rules = crate::risk::risky_rules(cmd);
    if rules.is_empty() {
        return Some(rules);
    }
    let Some(file) = risk_ack_file(pid) else {
        tmux_display_msg(&format!(
            "󰳌 risky ({}): answer it in the pane",
            rules.join(", ")
        ));
        return None;
    };
    if risk_ack_confirms(read_risk_ack(&file), prompt_key, now_ms) {
        let _ = std::fs::remove_file(&file);
        return Some(rules);
    }
    let _ = std::fs::write(&file, format!("{prompt_key} {}", now_ms + RISK_ACK_MS));
    tmux_display_msg_timed(
        u32::try_from(RISK_ACK_MS).unwrap_or(3000),
        &format!(
            "󰳌 risky ({}): press y again within 3s to allow {}",
            rules.join(", "),
            one_line_excerpt(cmd, 60)
        ),
    );
    None
}

/// What the user reads for a refused answer to a held prompt.
pub fn held_refusal(code: &str) -> &'static str {
    match code {
        "too_soon" => "prompt just appeared: read it, then press again",
        "risk_ack_required" => "is a risky call: press y twice",
        _ => "prompt changed or ended: look again",
    }
}

/// Answers a prompt the hook holds off the pane: no keystroke reaches the pane.
fn answer_held(pid: &str, held: &serde_json::Value, approve: bool, now: u64) {
    let (Some(id), Some(line)) = (held["id"].as_u64(), held["line"].as_str()) else {
        return;
    };
    let mut risk_ack = Vec::new();
    if approve {
        let cmd = (held["tool"] == "Bash").then_some(line);
        let key = held["created_at"].as_u64().unwrap_or(0);
        match risk_gate(pid, cmd, key, now) {
            Some(rules) => risk_ack = rules,
            None => return,
        }
    }
    let params = serde_json::json!({
        "pane_id": pid,
        "id": id,
        "decision": if approve { "allow" } else { "deny" },
        "line": line,
        "risk_ack": risk_ack,
    });
    match crate::rpc_client::rpc_call("approval/answer", &params, 500) {
        Ok(_) => {}
        Err(crate::rpc_client::CallError::Rejected(code)) => {
            tmux_display_msg(&format!(
                "Action ignored: Pane {pid} {}",
                held_refusal(&code)
            ));
        }
        Err(crate::rpc_client::CallError::Unreachable) => {
            tmux_display_msg("Action ignored: waymux daemon is not running");
        }
    }
}

fn answer(pid: &str, row_state: &str, approve: bool) {
    let live = crate::rpc_client::fetch_agent_states();
    let entry = live.get(pid);
    let now = crate::sys::unix_ms();
    if let Err(why) = confirm_check(
        row_state,
        entry.map(|e| (e.state.as_str(), e.changed_at)),
        now,
    ) {
        tmux_display_msg(&format!("Action ignored: Pane {pid} {why}"));
        return;
    }
    if let Ok(held) =
        crate::rpc_client::rpc_call("approval/get", &serde_json::json!({ "pane_id": pid }), 150)
        && !held.is_null()
    {
        answer_held(pid, &held, approve, now);
        return;
    }
    if approve {
        let cmd = pending_command(pid, entry.and_then(|e| e.message.as_deref()));
        if risk_gate(pid, cmd.as_deref(), entry.map_or(0, |e| e.changed_at), now).is_none() {
            return;
        }
    }
    let harness = tmux_out(&[
        "display-message",
        "-p",
        "-t",
        pid,
        "#{pane_current_command}",
    ])
    .unwrap_or_default();
    let mut args = vec!["send-keys", "-t", pid];
    args.extend_from_slice(answer_keys(harness.trim(), approve));
    let _ = tmux_status(&args);
}

pub fn run_agent_action(args: &[String]) -> i32 {
    let action = args.first().map_or("", std::string::String::as_str);
    let state = args.get(1).map_or("", std::string::String::as_str);
    let pid = args.get(2).map_or("", std::string::String::as_str);

    if !is_valid_pane_id(pid) {
        return 0;
    }

    match action {
        "approve" => {
            answer(pid, state, true);
            0
        }
        "deny" => {
            answer(pid, state, false);
            0
        }
        "prompt" => {
            if args.len() > 3 {
                let prompt_text = args[3..].join(" ");
                if !prompt_text.is_empty() {
                    let outcome = crate::delivery::dispatch_prompt(pid, &prompt_text, "cockpit");
                    tmux_display_msg(&outcome.message(pid));
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
        let target_sess = format!("{session}:");
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

    let target_sess = format!("{session}:");
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
        println!("{target}");
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

/// Returns true when the kill may proceed; otherwise arms `key` and tells the user why.
fn confirm_kill(key: &str, label: &str, states: &[String]) -> bool {
    let now = crate::sys::unix_ms();
    let armed = tmux_get_global_opt(KILL_ARM_OPT);
    match kill_decision(!states.is_empty(), &armed, key, now) {
        KillDecision::Kill => {
            if !armed.is_empty() {
                tmux_unset_global_opt(KILL_ARM_OPT);
            }
            true
        }
        KillDecision::Arm => {
            tmux_set_global_opt(KILL_ARM_OPT, &format!("{key}|{now}"));
            tmux_display_msg_timed(
                u32::try_from(KILL_CONFIRM_MS).unwrap_or(u32::MAX),
                &format!(
                    " 󰚌 {}: agent {} │ press again to kill",
                    label,
                    states.first().map_or("active", String::as_str)
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

        let target_sess = format!("{sess_name}:");
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
        let target = format!("{sess}:{idx}");
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
        let win_count = tmux_out(&["list-windows", "-t", &sess]).map_or(0, |s| s.lines().count());

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
    fn test_confirm_check_prefers_live_state_and_waits_dwell() {
        // Row says permission, daemon says the agent already moved on.
        assert!(confirm_check("permission", Some(("working", 1)), 10_000).is_err());
        // Row is stale the other way: daemon says permission, long enough ago.
        assert!(confirm_check("idle", Some(("permission", 1_000)), 10_000).is_ok());
        // Prompt arrived 100 ms ago: too soon.
        let err = confirm_check("permission", Some(("permission", 9_900)), 10_000).unwrap_err();
        assert!(err.contains("just appeared"));
        // Exactly the dwell is enough.
        assert!(confirm_check("question", Some(("question", 10_000 - DWELL_MS)), 10_000).is_ok());
        // Unknown start (rehydrated) and no daemon entry fall back to the row.
        assert!(confirm_check("permission", Some(("permission", 0)), 10_000).is_ok());
        assert!(confirm_check("permission", None, 10_000).is_ok());
        assert!(confirm_check("working", None, 10_000).is_err());
    }

    #[test]
    fn test_answer_keys_never_deny_with_enter() {
        assert_eq!(answer_keys("claude", true), ["1"]);
        for h in ["claude", "codex", "agy", "gemini", ""] {
            let deny = answer_keys(h, false);
            assert_eq!(deny, ["Escape"], "{h}");
            assert!(
                !deny.contains(&"Enter"),
                "{h}: Enter confirms the highlighted Yes"
            );
        }
        assert_eq!(answer_keys("codex", true), ["y", "Enter"]);
    }

    #[test]
    fn test_risk_ack_needs_same_prompt_within_window() {
        assert!(!risk_ack_confirms(None, 5, 1_000));
        assert!(risk_ack_confirms(Some((5, 4_000)), 5, 3_999));
        assert!(risk_ack_confirms(Some((5, 4_000)), 5, 4_000));
        assert!(!risk_ack_confirms(Some((5, 4_000)), 5, 4_001), "expired");
        assert!(
            !risk_ack_confirms(Some((5, 4_000)), 6, 3_000),
            "another prompt"
        );
    }

    #[test]
    fn test_one_line_excerpt() {
        assert_eq!(one_line_excerpt("rm   -rf\n build", 60), "rm -rf build");
        assert_eq!(one_line_excerpt("abcdef", 3), "abc…");
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
