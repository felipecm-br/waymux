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
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("WAYMUX_COLORS");
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
    let out = sb.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("[FAIL] daemon"), "{text}");
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

    assert!(daemon.stop().success());
}
