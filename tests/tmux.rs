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
    assert!(!answer.exists(), "approve typed into an idle pane");
    // A malformed pane id is ignored.
    env.run(&["agent-action", "approve", "permission", "main:0"]);

    env.run(&["agent-action", "approve", "permission", &pane]);
    assert!(eventually(
        || std::fs::read_to_string(&answer).is_ok_and(|a| a.trim() == "y")
    ));
    let _ = std::fs::remove_file(&answer);
}

/// Deny sends `Esc` and never `Enter`: in Claude Code `n` is not a hotkey and `Enter`
/// confirms the highlighted "1. Yes", so the old `n` + `Enter` approved the call.
#[test]
fn deny_sends_escape_and_no_enter() {
    let answer = std::env::temp_dir().join(format!("waymux-deny-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer);
    // The pane records, in hex, every byte it reads in the first second.
    let cmd = format!(
        "bash -c 'IFS= read -rsn1 a; IFS= read -rs -t 1 b; printf %s \"$a$b\" | od -An -tx1 > \"{}\"; sleep 30'",
        answer.display()
    );
    tmux_or_skip!(env, tmux, &cmd);
    let pane = first_pane(&tmux);
    env.run(&["agent-action", "deny", "question", &pane]);
    assert!(eventually(|| std::fs::read_to_string(&answer).is_ok_and(
        |a| a.split_whitespace().collect::<Vec<_>>() == ["1b"]
    )));
    let _ = std::fs::remove_file(&answer);
}

/// A risky call is typed only on the second `y` within 3 s.
#[test]
fn risky_approve_needs_a_second_press() {
    let answer = std::env::temp_dir().join(format!("waymux-risky-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer);
    let cmd = format!("read a; echo \"$a\" > '{}'; sleep 30", answer.display());
    tmux_or_skip!(env, tmux, &cmd);
    let pane = first_pane(&tmux);
    // What the hook remembers for the prompt on this pane.
    let memo_dir = env.dir.join("run/waymux");
    std::fs::create_dir_all(&memo_dir).unwrap();
    std::fs::set_permissions(&memo_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let safe: String = pane
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::fs::write(
        memo_dir.join(format!("pane-cmd-{safe}")),
        "cd build && rm -rf out",
    )
    .unwrap();

    env.run(&["agent-action", "approve", "permission", &pane]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!answer.exists(), "first press only arms a risky approval");

    env.run(&["agent-action", "approve", "permission", &pane]);
    assert!(eventually(
        || std::fs::read_to_string(&answer).is_ok_and(|a| a.trim() == "y")
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

/// A fake agent named `name` (the kernel names a `#!/bin/bash` script's process after the
/// file): it draws `screen`, then appends every line it reads to `<env>/<name>.in`.
fn fake_agent(env: &Env, name: &str, screen: &str) -> std::path::PathBuf {
    // tmux reports a script's interpreter (`bash`); switch then finds the agent with `ps`.
    let ps = env.dir.join("bin/ps");
    if !ps.exists() {
        std::os::unix::fs::symlink(common::which("ps"), ps).unwrap();
    }
    let path = env.dir.join("bin").join(name);
    let input = env.dir.join(format!("{name}.in"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/bash\nprintf '%b\\n' '{screen}'\nwhile IFS= read -r l; do printf '%s\\n' \"$l\" >> '{}'; done\n",
            input.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    input
}

/// `switch --agent` for a pane, empty until an agent runs there.
fn agent_line(env: &Env, pane: &str) -> String {
    let out = env
        .command(&["switch", "-t", pane, "--agent"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

#[test]
fn switch_detects_the_pane_agent_and_types_its_slash_command() {
    let env = Env::new();
    let input = fake_agent(&env, "claude", "  Opus 5.5 · ctx 1%\\n❯ ");
    let Some(tmux) = env.tmux(&env.dir.join("bin/claude").display().to_string()) else {
        eprintln!("tmux not installed; skipping");
        return;
    };
    let pane = first_pane(&tmux);
    assert!(eventually(|| agent_line(&env, &pane) == "claude\topus"));
    let rows = env.run(&["switch", "-t", &pane, "--peek"]);
    assert!(
        rows.lines().any(|l| l.starts_with("opus\t/model opus\t⠛")),
        "{rows}"
    );

    env.run(&["switch", "-t", &pane, "haiku"]);
    assert!(eventually(|| std::fs::read_to_string(&input)
        .unwrap_or_default()
        == "/model haiku\n"));
}

#[test]
fn switch_stops_on_a_half_typed_prompt() {
    let env = Env::new();
    let input = fake_agent(&env, "claude", "  Sonnet 5.5\\n❯ half typed");
    let Some(tmux) = env.tmux(&env.dir.join("bin/claude").display().to_string()) else {
        eprintln!("tmux not installed; skipping");
        return;
    };
    let pane = first_pane(&tmux);
    assert!(eventually(|| agent_line(&env, &pane) == "claude\tsonnet"));
    let out = env
        .command(&["switch", "-t", &pane, "high"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!input.exists(), "typed into a pane with a draft");
    let none = env
        .command(&["switch", "-t", "%99", "--agent"])
        .output()
        .unwrap();
    assert_eq!(none.status.code(), Some(1));
}

/// A provider with one agent, `other`, recommended on `--model m1`.
const PROVIDER: &str = r#"printf '[{"agent":"other","available":true,"level":"green","launch":[{"args":"--model m1","model":"M1","rec":true}]}]'"#;

#[test]
fn launch_tags_the_pane_and_the_popup_starts_the_pick_in_a_new_window() {
    tmux_or_skip!(env, tmux, "sleep 30");
    std::os::unix::fs::symlink("/bin/sh", env.dir.join("bin/sh")).unwrap();
    // A fake agent that records its directory and arguments.
    let started = env.dir.join("started");
    let agent = env.dir.join("bin/other");
    std::fs::write(
        &agent,
        format!("#!/bin/sh\necho \"$PWD $*\" > '{}'\n", started.display()),
    )
    .unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pane = first_pane(&tmux);

    // The pane records how its agent started.
    let out = env
        .command(&["launch", "other", "--model", "x"])
        .env("TMUX_PANE", &pane)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&started);
    let tag = tmux.run(&["show-options", "-p", "-t", &pane, "-v", "@ai_launch"]);
    assert_eq!(tag, "other --model x");

    // Inside the popup: no terminal, so the pick is the recommended row; it starts in a new
    // window of the origin session, rooted at the directory.
    let before = tmux.window_count("main");
    let dir = env.repo();
    let out = env
        .command(&["launch", "--popup", &dir.display().to_string()])
        .env(
            "TMUX",
            format!(
                "{},0,0",
                tmux.run(&["display-message", "-p", "#{socket_path}"])
            ),
        )
        .env("TMUX_POPUP", "1")
        .env("TMUX_ORIGIN_SESSION", "main")
        .env("WAYMUX_QUOTA_PROVIDERS", PROVIDER)
        .env("WAYMUX_QUOTA_PREFERENCE", "other")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        eventually(|| std::fs::read_to_string(&started).unwrap_or_default()
            == format!("{} --model m1\n", dir.display())),
        "the new window never ran the pick: {:?}",
        std::fs::read_to_string(&started)
    );
    assert!(tmux.window_count("main") >= before);
}
