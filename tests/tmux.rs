//! End-to-end tests of the commands that drive tmux (`agent-action`, `window-create`,
//! `window-delete`, `triage --jump`) against a private tmux server per test. Skipped when
//! tmux is not installed.
//!
//! The harness clears the environment, so these run with no UTF-8 locale: that is how they
//! caught tmux turning the tabs of `-F` rows into `_` without `tmux -u`.

mod common;

use common::{Env, Tmux, eventually};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

/// The env and its tmux server, or None (test skipped) without tmux.
macro_rules! tmux_or_skip {
    ($env:ident, $tmux:ident, $cmd:expr) => {
        let $env = Env::new();
        let Some($tmux) = $env.tmux($cmd) else {
            eprintln!("tmux not installed; skipping");
            return;
        };
    };
}

fn run_with_stdin(env: &Env, args: &[&str], stdin: &str) -> String {
    let mut child = env
        .command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn first_pane(tmux: &Tmux) -> String {
    tmux.run(&["display-message", "-p", "-t", "main:", "#{pane_id}"])
}

#[test]
fn approve_and_deny_type_into_a_waiting_pane_only() {
    // The pane records the first line it reads.
    let answer = std::env::temp_dir().join(format!("waymux-answer-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer);
    let cmd = format!("read a; echo \"$a\" > '{}'; sleep 30", answer.display());
    tmux_or_skip!(env, tmux, &cmd);
    let pane = first_pane(&tmux);

    // Not awaiting confirmation: nothing is typed.
    env.run(&["agent-action", "approve", "idle", &pane]);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!answer.exists(), "approve typed into an idle pane");
    // A malformed pane id is ignored.
    env.run(&["agent-action", "approve", "permission", "main:0"]);

    env.run(&["agent-action", "approve", "permission", &pane]);
    assert!(eventually(
        || std::fs::read_to_string(&answer).is_ok_and(|a| a.trim() == "y")
    ));
    let _ = std::fs::remove_file(&answer);
}

#[test]
fn deny_answers_n() {
    let answer = std::env::temp_dir().join(format!("waymux-deny-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer);
    let cmd = format!("read a; echo \"$a\" > '{}'; sleep 30", answer.display());
    tmux_or_skip!(env, tmux, &cmd);
    let pane = first_pane(&tmux);
    env.run(&["agent-action", "deny", "question", &pane]);
    assert!(eventually(
        || std::fs::read_to_string(&answer).is_ok_and(|a| a.trim() == "n")
    ));
    let _ = std::fs::remove_file(&answer);
}

#[test]
fn window_create_opens_a_window_in_the_header_session() {
    tmux_or_skip!(env, tmux, "sleep 30");
    assert_eq!(tmux.window_count("main"), 1);
    let target = run_with_stdin(&env, &["window-create"], "#main\n");
    assert!(target.starts_with("main:"), "{target:?}");
    assert_eq!(tmux.window_count("main"), 2);
}

#[test]
fn window_delete_kills_an_idle_window_at_once_and_an_active_one_on_the_second_press() {
    tmux_or_skip!(env, tmux, "sleep 30");
    tmux.run(&["new-window", "-d", "-t", "main:1", "sleep 30"]);
    tmux.run(&["new-window", "-d", "-t", "main:2", "sleep 30"]);
    assert_eq!(tmux.window_count("main"), 3);

    // Cockpit rows: column 1 is the window index, column 3 the session.
    env.run(&["window-delete"]); // empty input is a no-op
    run_with_stdin(&env, &["window-delete"], "x\t1\tx\tmain\n");
    assert_eq!(tmux.window_count("main"), 2, "idle window killed at once");

    let busy = tmux.run(&["display-message", "-p", "-t", "main:2", "#{pane_id}"]);
    tmux.run(&[
        "set-option",
        "-p",
        "-t",
        &busy,
        "@ai_agent_state_raw",
        "busy",
    ]);
    run_with_stdin(&env, &["window-delete"], "x\t2\tx\tmain\n");
    assert_eq!(tmux.window_count("main"), 2, "first press only arms");
    assert!(
        tmux.run(&["show-option", "-gv", "@cockpit_kill_armed"])
            .starts_with("main:2|")
    );
    run_with_stdin(&env, &["window-delete"], "x\t2\tx\tmain\n");
    assert_eq!(tmux.window_count("main"), 1, "second press kills");
}

#[test]
fn triage_jump_visits_the_waiting_pane() {
    tmux_or_skip!(env, tmux, "sleep 30");
    tmux.run(&["new-window", "-d", "-t", "main:1", "sleep 30"]);
    let waiting = tmux.run(&["display-message", "-p", "-t", "main:1", "#{pane_id}"]);
    // No daemon: triage falls back to the states recorded on panes.
    tmux.run(&[
        "set-option",
        "-p",
        "-t",
        &waiting,
        "@ai_agent_state_raw",
        "permission",
    ]);
    assert_eq!(
        tmux.run(&["display-message", "-p", "-t", "main", "#I"]),
        "0"
    );

    env.run(&["triage", "--jump"]);
    assert_eq!(
        tmux.run(&["display-message", "-p", "-t", "main", "#I"]),
        "1",
        "the waiting window is selected"
    );
}

#[test]
fn window_delete_runs_the_record_script_before_killing() {
    tmux_or_skip!(env, tmux, "sleep 30");
    tmux.run(&["new-window", "-d", "-t", "main:1", "sleep 30"]);
    let scripts = env.dir.join("home/.config/waymaker/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    let script = scripts.join("record-window-state.sh");
    std::fs::write(&script, "#!/bin/sh\necho \"$@\" > \"$HOME/recorded\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    run_with_stdin(&env, &["window-delete"], "x\t1\tx\tmain\n");
    assert_eq!(
        std::fs::read_to_string(env.dir.join("home/recorded"))
            .unwrap()
            .trim(),
        "--push main:1"
    );
    assert_eq!(tmux.window_count("main"), 1);
}

#[test]
fn hook_and_inject_paste_into_the_workspace_agent_pane_and_clear_on_session_end() {
    let answer = std::env::temp_dir().join(format!("waymux-inject-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer);
    let cmd = format!("read a; echo \"$a\" > '{}'; sleep 30", answer.display());
    tmux_or_skip!(env, tmux, &cmd);
    let pane = first_pane(&tmux);
    let repo = env.repo();
    let repo_str = repo.to_string_lossy();

    // Empty workspace or prompt exits 1.
    assert_eq!(
        env.command(&["inject", "", ""])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    // Without a matching agent pane, inject exits 1.
    assert_eq!(
        env.command(&["inject", "/nonexistent", "hi"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );

    // Run tmux and lazygit lifecycle hooks on the pane: sets @ai_agent_title, @ai_agent_state_raw,
    // and records the workspace's active pane file.
    tmux.run(&[
        "set-option",
        "-p",
        "-t",
        &pane,
        "@ai_agent_state_raw",
        "idle",
    ]);
    let run_hook = |kind: &str, event: &str, body: &str| {
        let mut child = env
            .command(&["hook", kind, event, "claude"])
            .env("TMUX_PANE", &pane)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    };
    run_hook("tmux", "SessionStart", r#"{"source":"startup"}"#);
    run_hook(
        "tmux",
        "UserPromptSubmit",
        r#"{"prompt":"Fix the login flow"}"#,
    );
    assert_eq!(
        tmux.run(&["show-option", "-wv", "-t", &pane, "@ai_agent_title"]),
        "Fix the login flow"
    );
    run_hook(
        "tmux",
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    run_hook("tmux", "PermissionRequest", r#"{"message":"allow?"}"#);
    run_hook("tmux", "PostToolUse", "{}");
    run_hook("tmux", "Stop", "{}");
    run_hook(
        "lazygit",
        "PreToolUse",
        &format!(r#"{{"sessionId":"s1","cwd":"{repo_str}"}}"#),
    );

    // inject finds the pane via the recorded active-pane file and pastes with bracketed paste.
    env.run(&["inject", &repo_str, "review note from lazygitrs"]);
    assert!(eventually(|| {
        std::fs::read_to_string(&answer).is_ok_and(|a| a.contains("review note from lazygitrs"))
    }));

    // SessionEnd clears @ai_agent_title and lazygit sentinel.
    run_hook("tmux", "SessionEnd", "{}");
    run_hook(
        "lazygit",
        "SessionEnd",
        &format!(r#"{{"sessionId":"s1","cwd":"{repo_str}"}}"#),
    );
    assert_eq!(
        tmux.run(&["show-option", "-wv", "-t", &pane, "@ai_agent_title"]),
        ""
    );
    let _ = std::fs::remove_file(&answer);
}
