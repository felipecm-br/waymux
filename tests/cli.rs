//! End-to-end tests of the `waymux` binary: a real daemon on a free port, driven through the
//! CLI. Each daemon gets its own runtime dir, HOME and tmux socket dir, so it never touches
//! the developer's tmux server, sounds or token.

use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_waymux");

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    dir: PathBuf,
    /// Re-picked when a daemon dies before answering: the free port found at setup can be taken
    /// by another test before the daemon binds it.
    port: Cell<u16>,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "waymux-it-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tmux")).unwrap();
        let sb = Sandbox {
            dir,
            port: Cell::new(free_port()),
        };
        sb.write_config();
        sb
    }

    fn write_config(&self) {
        let port = self.port.get();
        let config = format!(
            "listen_addr = \"127.0.0.1\"\nport = {port}\npid_file = \"{}\"\n\
             shutdown_timeout_secs = 1\nidle_debounce_ms = 0\n\n[sound]\nenabled = false\n",
            self.dir.join("waymux.pid").display()
        );
        std::fs::write(self.dir.join("config.toml"), config).unwrap();
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("HOME", &self.dir)
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("TMUX_TMPDIR", self.dir.join("tmux"))
            .env("WAYMUX_PORT", self.port.get().to_string())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("WAYMUX_COLORS")
            .env_remove("CLAUDE_CONFIG_DIR");
        // A wrapper's quota, launch and switch settings (an agent started by `waymux launch`
        // through one) would point the tests at the developer's state files.
        for (name, _) in std::env::vars() {
            if ["WAYMUX_QUOTA_", "WAYMUX_LAUNCH_", "WAYMUX_SWITCH_"]
                .iter()
                .any(|p| name.starts_with(p))
            {
                cmd.env_remove(name);
            }
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run_with_stdin(&self, args: &[&str], envs: &[(&str, &str)], stdin: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Starts a daemon and waits for `health`. A daemon that exits first (its port was taken
    /// meanwhile) is retried on a fresh port; its stderr goes to a file, so a chatty daemon never
    /// blocks on a full pipe, and the file is quoted when the daemon never comes up.
    fn spawn_daemon(&self) -> Daemon {
        let log = self.dir.join("daemon.stderr");
        for _attempt in 0..3 {
            let mut child = self
                .command()
                .args(["daemon", "--config"])
                .arg(self.dir.join("config.toml"))
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if self.run(&["health"]).status.success() {
                    return Daemon { child };
                }
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
            self.port.set(free_port());
            self.write_config();
        }
        let mut stderr = String::new();
        let _ = std::fs::File::open(&log).and_then(|mut f| f.read_to_string(&mut stderr));
        panic!("daemon did not become healthy after 3 ports; last stderr:\n{stderr}");
    }

    fn list(&self) -> String {
        String::from_utf8_lossy(&self.run(&["list"]).stdout).to_string()
    }

    fn wait_for_list(&self, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = self.list();
            if pred(&out) || Instant::now() > deadline {
                return out;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn stop(mut self) -> std::process::ExitStatus {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.child.id()).unwrap());
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        let status = self.child.wait().unwrap();
        std::mem::forget(self);
        status
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn unknown_command_exits_2_without_starting_the_daemon() {
    let sb = Sandbox::new();
    let out = sb.run(&["stauts"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown command 'stauts'"));
    assert!(!stderr(&out).contains("Starting Waymux Daemon"));
    assert!(!sb.dir.join("waymux.pid").exists());
}

#[test]
fn subcommand_help_flag_prints_usage_instead_of_running() {
    let sb = Sandbox::new();
    let out = sb.run(&["status", "--help"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("USAGE:\n    waymux status"));
}

#[test]
fn clients_report_a_missing_daemon() {
    let sb = Sandbox::new();
    let out = sb.run(&["health"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not running or unreachable"));
    assert_eq!(sb.run(&["status"]).status.code(), Some(1));
}

#[test]
fn hook_states_flow_through_the_daemon_and_dismiss_clears_them() {
    let sb = Sandbox::new();
    let daemon = sb.spawn_daemon();
    assert!(
        sb.dir.join("waymux/token").exists(),
        "daemon writes its token"
    );

    let pane = [("TMUX_PANE", "%9001")];
    let out = sb.run_with_stdin(
        &["hook", "tmux", "PreToolUse"],
        &pane,
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    assert!(out.status.success(), "hook failed: {}", stderr(&out));
    let listed = sb.wait_for_list(|l| l.contains("%9001\t"));
    assert!(listed.contains("%9001\t"), "pane not listed: {listed:?}");

    let out = sb.run_with_stdin(
        &["hook", "tmux", "Notification", "claude"],
        &pane,
        r#"{"message":"Claude needs your permission to use Bash"}"#,
    );
    assert!(out.status.success());
    let listed = sb.wait_for_list(|l| l.contains("%9001\tpermission"));
    assert!(listed.contains("%9001\tpermission"), "{listed:?}");

    assert!(sb.run(&["dismiss", "%9001"]).status.success());
    let listed = sb.wait_for_list(|l| !l.contains("%9001\tpermission"));
    assert!(listed.contains("%9001\tidle"), "{listed:?}");

    assert!(daemon.stop().success());
    assert!(
        !sb.dir.join("waymux.pid").exists(),
        "PID file removed on shutdown"
    );
}

#[test]
fn second_daemon_is_refused_by_the_pid_file() {
    let sb = Sandbox::new();
    let daemon = sb.spawn_daemon();
    let out = sb
        .command()
        .args(["daemon", "--config"])
        .arg(sb.dir.join("config.toml"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("already running"), "{}", stderr(&out));
    drop(daemon);
}

#[test]
fn rpc_rejects_requests_without_the_token() {
    let sb = Sandbox::new();
    let _daemon = sb.spawn_daemon();
    let body = r#"{"jsonrpc":"2.0","method":"agentState/list","id":1}"#;
    let request = format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", sb.port.get())).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
}

#[test]
fn init_hooks_prints_valid_claude_settings_json() {
    let sb = Sandbox::new();
    let out = sb.run(&["init", "--hooks"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find('{').unwrap();
    let end = text.rfind("\n}").unwrap() + 2;
    let json: serde_json::Value = serde_json::from_str(&text[start..end]).unwrap();
    assert!(json["hooks"]["Stop"].is_array());
}

#[test]
fn doctor_names_the_missing_daemon_and_fails() {
    let sb = Sandbox::new();
    let cfg_dir = sb.dir.join(".config/waymux");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "flow_log_hok = \"/bin/true\"\n",
    )
    .unwrap();
    let out = sb.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("[FAIL] daemon"), "{text}");
    assert!(
        text.contains("[FAIL] config") && text.contains("flow_log_hok"),
        "{text}"
    );
}

#[test]
fn sighup_reloads_the_config_and_keeps_it_when_the_new_one_is_broken() {
    let sb = Sandbox::new();
    let daemon = sb.spawn_daemon();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(daemon.child.id()).unwrap());
    let hup = || nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGHUP).unwrap();

    // flow_log_hook is the reloadable setting a client can observe: it runs on a question.
    let events = sb.dir.join("events");
    let hook = sb.dir.join("hook.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho \"$1\" >> '{}'\n", events.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config_path = sb.dir.join("config.toml");
    let base = std::fs::read_to_string(&config_path).unwrap();
    let with_hook = base.replacen(
        "\n\n[sound]",
        &format!("\nflow_log_hook = \"{}\"\n\n[sound]", hook.display()),
        1,
    );

    // Toggle the pane between working and permission until the hook has logged a question.
    let pane = [("TMUX_PANE", "%9002")];
    let question_logged = || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            sb.run_with_stdin(&["hook", "tmux", "PreToolUse"], &pane, "{}");
            sb.run_with_stdin(
                &["hook", "tmux", "Notification", "claude"],
                &pane,
                r#"{"message":"Claude needs your permission to use Bash"}"#,
            );
            std::thread::sleep(Duration::from_millis(100));
            if std::fs::read_to_string(&events).is_ok_and(|e| e.contains("agent_question")) {
                return true;
            }
        }
        false
    };

    std::fs::write(&config_path, &with_hook).unwrap();
    hup();
    assert!(question_logged(), "SIGHUP did not apply flow_log_hook");

    std::fs::write(&config_path, "this is = not toml").unwrap();
    hup();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        sb.run(&["health"]).status.success(),
        "daemon died on a broken config"
    );
    std::fs::remove_file(&events).unwrap();
    assert!(
        question_logged(),
        "a broken config replaced the running one"
    );

    // An unknown key is also rejected on reload and keeps the running config; at startup it
    // exits non-zero naming the key and file.
    let with_typo = format!("{with_hook}\nflow_log_hok = \"/bin/false\"\n");
    std::fs::write(&config_path, &with_typo).unwrap();
    hup();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::remove_file(&events).unwrap();
    assert!(
        question_logged(),
        "a config with an unknown key replaced the running one"
    );

    assert!(daemon.stop().success());

    let bad_start = sb
        .command()
        .args(["daemon", "--config"])
        .arg(&config_path)
        .output()
        .unwrap();
    assert!(!bad_start.status.success());
    let err = stderr(&bad_start);
    assert!(
        err.contains("config.toml") && err.contains("flow_log_hok"),
        "{err}"
    );
}

#[test]
fn quota_records_the_statusline_and_merges_provider_agents() {
    let sb = Sandbox::new();
    let reset = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let payload = |five: u32| {
        format!(
            r#"{{"model":{{"display_name":"Opus"}},"rate_limits":{{"five_hour":{{"used_percentage":{five},"resets_at":{reset}}},"seven_day":{{"used_percentage":22,"resets_at":"2099-01-01T00:00:00.000Z"}}}}}}"#
        )
    };
    let out = sb.run_with_stdin(&["quota", "statusline"], &[], &payload(42));
    assert!(out.status.success(), "{}", stderr(&out));
    let line = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        line.starts_with("Opus · ") && line.contains("5h 42%") && line.contains("7d 22%"),
        "{line}"
    );
    let samples = sb.dir.join(".local/state/waymux/quota/claude-samples.tsv");
    assert_eq!(
        std::fs::read_to_string(&samples).unwrap().lines().count(),
        1
    );

    // Claude at its cap: the provider's agent is recommended; a broken provider is skipped.
    sb.run_with_stdin(&["quota", "statusline"], &[], &payload(99));
    let envs = [
        (
            "WAYMUX_QUOTA_PROVIDERS",
            "printf '[{\"agent\":\"other\",\"available\":true,\"level\":\"green\",\"note\":1}]'\necho not-json",
        ),
        ("WAYMUX_QUOTA_PREFERENCE", "claude other"),
    ];
    let out = sb.run_with_stdin(&["quota", "--json"], &envs, "");
    assert!(out.status.success(), "{}", stderr(&out));
    let snap: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(snap["agents"][0]["level"], "red", "{snap}");
    assert_eq!(snap["agents"][1]["agent"], "other");
    assert_eq!(snap["agents"][1]["note"], 1, "provider fields pass through");
    assert_eq!(snap["agents"].as_array().unwrap().len(), 2);
    assert_eq!(snap["recommend"], "other");
    let rec = sb.run_with_stdin(&["quota", "--recommend"], &envs, "");
    assert_eq!(String::from_utf8_lossy(&rec.stdout), "other\n");
}

#[test]
fn quota_refresh_is_silent_and_writes_the_badge_caches() {
    let sb = Sandbox::new();
    let out = sb.run(&["quota", "--refresh"]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    let state = sb.dir.join(".local/state/waymux/quota");
    assert!(
        std::fs::read_to_string(state.join("badge"))
            .unwrap()
            .contains("→")
    );
    assert!(state.join("badge-min").exists());
    let bad = sb.run(&["quota", "--bogus"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("unknown option '--bogus'"));
}

#[test]
fn launch_arms_a_choice_and_peeks_without_consuming_it() {
    let sb = Sandbox::new();
    assert!(
        sb.run(&["launch", "--next", "codex", "--model", "o4"])
            .status
            .success()
    );
    for _ in 0..2 {
        let out = sb.run(&["launch", "--peek"]);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "codex --model o4\n");
    }
    let bad = sb.run(&["launch", "--next"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn launch_rows_come_from_providers_filters_and_the_exec_hook() {
    let sb = Sandbox::new();
    let provider = r#"printf '[{"agent":"other","available":true,"level":"green","launch":[{"args":"--model m0","model":"M0"},{"args":"--model m1","model":"M1","rec":true}]}]'"#;
    let base = [
        ("WAYMUX_QUOTA_PROVIDERS", provider),
        ("WAYMUX_QUOTA_PREFERENCE", "other"),
    ];
    // No terminal: the menu falls back to the recommended row.
    let out = sb.run_with_stdin(&["launch", "--peek"], &base, "");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "other --model m1\n",
        "{}",
        stderr(&out)
    );

    let filtered = [base[0], base[1], ("WAYMUX_LAUNCH_FILTER", "sed s/m1/m2/")];
    let out = sb.run_with_stdin(&["launch", "--peek", "--auto"], &filtered, "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "other --model m2\n");

    let hooked = [("WAYMUX_LAUNCH_EXEC", "printf '%s|'")];
    let out = sb.run_with_stdin(&["launch", "other", "--model", "x y"], &hooked, "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "other|--model|x y|");
}

#[test]
fn switch_maps_choices_to_the_agent_slash_command() {
    let sb = Sandbox::new();
    let claude = [("WAYMUX_SWITCH_AGENT", "claude")];
    let out = sb.run_with_stdin(&["switch", "--peek", "high"], &claude, "");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "claude\t/effort high\n"
    );
    let out = sb.run_with_stdin(&["switch", "--peek", "sonnet"], &claude, "");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "claude\t/model sonnet\n"
    );
    let rows = sb.run_with_stdin(&["switch", "--peek"], &claude, "");
    let rows = String::from_utf8_lossy(&rows.stdout).to_string();
    assert!(
        rows.starts_with("opus\t/model opus\t") && rows.contains("auto\t/effort auto\t"),
        "{rows}"
    );
    let bad = sb.run_with_stdin(&["switch", "--peek", "nope"], &claude, "");
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("'nope' is not a model or effort of claude"));

    let hooked = [
        ("WAYMUX_SWITCH_AGENT", "other"),
        (
            "WAYMUX_SWITCH_ROWS",
            r#"printf '{"agent":"other","current":"M","rows":[{"id":"m","command":"/model m","name":"M","active":true}]}' #"#,
        ),
    ];
    let out = sb.run_with_stdin(&["switch", "--agent"], &hooked, "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "other\tM\n");
    let none = sb.run_with_stdin(
        &["switch", "--agent"],
        &[("WAYMUX_SWITCH_AGENT", "other")],
        "",
    );
    assert_eq!(
        none.status.code(),
        Some(1),
        "no rows command: no agent menu"
    );
}

#[test]
fn launch_keeps_its_settings_out_of_the_agent() {
    let sb = Sandbox::new();
    let envs = [
        ("WAYMUX_QUOTA_STATE_DIR", "/elsewhere"),
        (
            "WAYMUX_LAUNCH_EXEC",
            r#"printf '%s|%s' "${WAYMUX_QUOTA_STATE_DIR-unset}" "${WAYMUX_LAUNCH_EXEC-unset}" #"#,
        ),
    ];
    let out = sb.run_with_stdin(&["launch", "other"], &envs, "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "unset|unset");
}

#[test]
fn quota_popup_outside_tmux_refreshes_and_closes_on_keys() {
    let sb = Sandbox::new();
    let out = sb.run_with_stdin(&["quota", "--popup", "/tmp"], &[], "rq");
    assert!(out.status.success(), "{}", stderr(&out));
    let screen = String::from_utf8_lossy(&out.stdout);
    assert!(
        screen.contains("Recommended") && screen.contains("refreshing"),
        "{screen}"
    );
    assert!(
        sb.dir.join(".local/state/waymux/quota/badge").exists(),
        "r ran --refresh"
    );
}
