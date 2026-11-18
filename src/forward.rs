//! `waymux forward`: open a port of a remote agent's host on this machine.
//!
//! An agent running over SSH in a pane starts a dev server on the remote host's `localhost`; the
//! browser here cannot reach it. `waymux forward <pane> <port>` finds the `ssh` running in that
//! pane, opens a second `ssh -N -L` with the same destination and options, and records it, so
//! `list` shows it and it is closed with `close` or, when the pane is gone, by the next `list`.
//! `ports <pane>` asks the remote host which TCP ports listen. Idea from Rex's `rex forward`
//! and the port forwarding of its 16 Nov 2026 demo; nothing is forwarded on its own here.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const USAGE: &str = "\
usage: waymux forward <pane> <port> [--local <port>]   Forward a remote port of the pane's ssh host
       waymux forward ports <pane>                    TCP ports listening on that host
       waymux forward list [--json]                   Open forwards (closes those whose pane is gone)
       waymux forward close <pane> [<port>]           Close a pane's forwards

  The pane must run `ssh` (directly or under its shell). The forward reuses that ssh's
  destination and options (-p, -l, -i, -J, -F, -o ...) with BatchMode, so it needs a key or
  agent, not a password; a ControlMaster in ~/.ssh/config makes it instant.";

/// ssh options that take a value (`ssh(1)`).
const WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";
/// Options that describe the interactive session, not the connection: not carried over.
const DROPPED: &str = "LRDWOTtNfn";

/// The connection part of an ssh command line: the options to keep and the destination. None
/// when no destination is found.
pub fn ssh_base(argv: &[String]) -> Option<(Vec<String>, String)> {
    let mut keep = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            return argv.get(i + 1).map(|d| (keep, d.clone()));
        }
        let Some(flags) = a.strip_prefix('-').filter(|f| !f.is_empty()) else {
            return Some((keep, a.clone()));
        };
        let mut word = String::from("-");
        let mut value = None;
        for (j, c) in flags.char_indices() {
            if WITH_ARG.contains(c) {
                let rest = &flags[j + c.len_utf8()..];
                if rest.is_empty() {
                    i += 1;
                    value = Some(argv.get(i)?.clone());
                } else {
                    value = Some(rest.to_string());
                }
                if word.len() > 1 {
                    keep.push(std::mem::take(&mut word));
                }
                if !DROPPED.contains(c) {
                    keep.push(format!("-{c}"));
                    keep.push(value.clone().unwrap_or_default());
                }
                break;
            }
            if !DROPPED.contains(c) {
                word.push(c);
            }
        }
        if value.is_none() && word.len() > 1 {
            keep.push(word);
        }
        i += 1;
    }
    None
}

/// The ports in `ss -ltnH` output, sorted and unique.
pub fn listening_ports(ss: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = ss
        .lines()
        .filter_map(|l| l.split_whitespace().nth(3))
        .filter_map(|local| local.rsplit_once(':'))
        .filter_map(|(_, p)| p.parse().ok())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Forward {
    pub pane: String,
    pub port: u16,
    pub local: u16,
    pub pid: i32,
    pub destination: String,
}

fn dir() -> Option<PathBuf> {
    let d = crate::presets::private_runtime_dir()?.join("forwards");
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn record_path(pane: &str, port: u16) -> Option<PathBuf> {
    Some(dir()?.join(format!("{}-{port}.json", pane.trim_start_matches('%'))))
}

fn records() -> Vec<(PathBuf, Forward)> {
    let Some(d) = dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(d) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let f = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
            Some((e.path(), f))
        })
        .collect()
}

fn alive(pid: i32) -> bool {
    pid > 0 && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

fn stop(path: &PathBuf, f: &Forward) {
    if alive(f.pid) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(f.pid),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let _ = std::fs::remove_file(path);
}

/// `pid ppid comm` rows of every process.
fn process_table() -> Vec<(i32, i32, String)> {
    let Ok(out) = Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid=", "-o", "comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((
                f.next()?.parse().ok()?,
                f.next()?.parse().ok()?,
                f.next()?.to_string(),
            ))
        })
        .collect()
}

/// The first `ssh` at or under `root` in `table`, breadth first.
pub fn find_ssh(table: &[(i32, i32, String)], root: i32) -> Option<i32> {
    let mut queue = std::collections::VecDeque::from([root]);
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if table
            .iter()
            .any(|(p, _, c)| *p == pid && c.rsplit('/').next() == Some("ssh"))
        {
            return Some(pid);
        }
        queue.extend(
            table
                .iter()
                .filter(|(_, pp, _)| *pp == pid)
                .map(|(p, _, _)| *p),
        );
    }
    None
}

fn argv_of(pid: i32) -> Option<Vec<String>> {
    if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let words: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|w| !w.is_empty())
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect();
        return (!words.is_empty()).then_some(words);
    }
    // Without /proc (macOS): ps loses quoting, which ssh options rarely need.
    let out = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let words: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    (!words.is_empty()).then_some(words)
}

/// The ssh connection of the pane: options and destination.
fn pane_ssh(pane: &str) -> Result<(Vec<String>, String), String> {
    let pid: i32 = crate::tmux_cmd::tmux_out(&["display-message", "-p", "-t", pane, "#{pane_pid}"])
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("no such pane: {pane}"))?;
    let ssh = find_ssh(&process_table(), pid).ok_or_else(|| format!("{pane} runs no ssh"))?;
    let argv = argv_of(ssh).ok_or("cannot read the ssh command line")?;
    ssh_base(&argv).ok_or_else(|| "no destination in the ssh command line".to_string())
}

fn ssh_command(opts: &[String]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .args(opts);
    cmd
}

fn open(pane: &str, port: u16, local: u16) -> i32 {
    let (opts, dest) = match pane_ssh(pane) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("waymux forward: {e}");
            return crate::verbs::exit::NOT_FOUND;
        }
    };
    if let Some((path, old)) = records()
        .into_iter()
        .find(|(_, f)| f.pane == pane && f.port == port)
    {
        if alive(old.pid) {
            println!("http://localhost:{}", old.local);
            return 0;
        }
        let _ = std::fs::remove_file(path);
    }
    let mut cmd = ssh_command(&opts);
    cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
        .arg(format!("127.0.0.1:{local}:127.0.0.1:{port}"))
        .arg(&dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Its own process group, so a Ctrl+C in the caller's terminal does not end the forward.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("waymux forward: ssh: {e}");
            return 1;
        }
    };
    // A refused bind or login fails within moments; give it that long before reporting.
    std::thread::sleep(std::time::Duration::from_millis(800));
    if let Ok(Some(status)) = child.try_wait() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut err);
        }
        eprintln!("waymux forward: ssh exited ({status}): {}", err.trim());
        return 1;
    }
    let Ok(pid) = i32::try_from(child.id()) else {
        return 1;
    };
    let f = Forward {
        pane: pane.to_string(),
        port,
        local,
        pid,
        destination: dest,
    };
    if let Some(path) = record_path(pane, port) {
        let _ = std::fs::write(path, serde_json::to_string(&f).unwrap_or_default());
    }
    println!("http://localhost:{local}");
    0
}

fn ports(pane: &str) -> i32 {
    let (opts, dest) = match pane_ssh(pane) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("waymux forward: {e}");
            return crate::verbs::exit::NOT_FOUND;
        }
    };
    let out = ssh_command(&opts)
        .arg(&dest)
        .args(["ss", "-ltnH"])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            for p in listening_ports(&String::from_utf8_lossy(&o.stdout)) {
                println!("{p}");
            }
            0
        }
        Ok(o) => {
            eprintln!(
                "waymux forward: `ss -ltnH` on {dest} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            1
        }
        Err(e) => {
            eprintln!("waymux forward: ssh: {e}");
            1
        }
    }
}

fn list(json: bool) -> i32 {
    let live: std::collections::HashSet<String> =
        crate::tmux_cmd::tmux_out(&["list-panes", "-a", "-F", "#{pane_id}"])
            .map(|o| o.lines().map(str::to_string).collect())
            .unwrap_or_default();
    let mut open = Vec::new();
    for (path, f) in records() {
        // The forward ends with its pane; an empty list (no server reached) closes nothing.
        if !alive(f.pid) || (!live.is_empty() && !live.contains(&f.pane)) {
            stop(&path, &f);
        } else {
            open.push(f);
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(&open).unwrap_or_else(|_| "[]".into())
        );
    } else {
        for f in &open {
            println!(
                "{}\t{}\thttp://localhost:{}\t{}",
                f.pane, f.port, f.local, f.destination
            );
        }
    }
    0
}

fn close(pane: &str, port: Option<u16>) -> i32 {
    let mut n = 0;
    for (path, f) in records() {
        if f.pane == pane && port.is_none_or(|p| p == f.port) {
            stop(&path, &f);
            n += 1;
        }
    }
    if n == 0 {
        eprintln!("waymux forward: no forward for {pane}");
        return 1;
    }
    0
}

fn port_arg(s: &str) -> Option<u16> {
    s.parse().ok().filter(|p| *p > 0)
}

pub fn run_cli(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["-h" | "--help", ..] => {
            println!("{USAGE}");
            i32::from(a.is_empty()) * 2
        }
        ["list"] => list(false),
        ["list", "--json"] => list(true),
        ["ports", pane] => ports(pane),
        ["close", pane] => close(pane, None),
        ["close", pane, port] => match port_arg(port) {
            Some(p) => close(pane, Some(p)),
            None => crate::verbs::exit::USAGE,
        },
        [pane, port] => match port_arg(port) {
            Some(p) => open(pane, p, p),
            None => crate::verbs::exit::USAGE,
        },
        [pane, port, "--local", local] => match (port_arg(port), port_arg(local)) {
            (Some(p), Some(l)) => open(pane, p, l),
            _ => crate::verbs::exit::USAGE,
        },
        _ => {
            eprintln!("waymux forward: see --help");
            crate::verbs::exit::USAGE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn the_connection_options_and_destination_are_kept_and_the_session_ones_dropped() {
        let (opts, dest) = ssh_base(&s(&[
            "ssh",
            "-t",
            "-p",
            "2222",
            "-i~/.ssh/k",
            "-AL",
            "8080:x:80",
            "-o",
            "Compression=yes",
            "me@box",
            "tmux",
            "attach",
        ]))
        .unwrap();
        assert_eq!(dest, "me@box");
        assert_eq!(
            opts,
            s(&[
                "-p",
                "2222",
                "-i",
                "~/.ssh/k",
                "-A",
                "-o",
                "Compression=yes"
            ])
        );
        assert_eq!(
            ssh_base(&s(&["ssh", "-J", "jump", "--", "host"]))
                .unwrap()
                .1,
            "host"
        );
        assert_eq!(ssh_base(&s(&["ssh", "-p"])), None);
        assert_eq!(ssh_base(&s(&["ssh", "-v"])), None);
    }

    #[test]
    fn listening_ports_come_from_the_local_address_column() {
        let ss = "LISTEN 0 511 127.0.0.1:5173 0.0.0.0:*\n\
                  LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\n\
                  LISTEN 0 128 [::]:22 [::]:*\n\
                  LISTEN 0 511 *:3000 *:*\n\
                  garbage\n";
        assert_eq!(listening_ports(ss), vec![22, 3000, 5173]);
    }

    #[test]
    fn the_ssh_is_found_under_the_pane_shell() {
        let table = vec![
            (10, 1, "zsh".to_string()),
            (11, 10, "tmux".to_string()),
            (12, 11, "/usr/bin/ssh".to_string()),
            (20, 1, "ssh".to_string()),
        ];
        assert_eq!(find_ssh(&table, 10), Some(12));
        assert_eq!(find_ssh(&table, 12), Some(12));
        assert_eq!(find_ssh(&table, 99), None);
    }

    #[test]
    fn bad_arguments_are_usage_errors() {
        assert_eq!(run_cli(&s(&["%1", "notaport"])), crate::verbs::exit::USAGE);
        assert_eq!(run_cli(&s(&["%1", "0"])), crate::verbs::exit::USAGE);
        assert_eq!(
            run_cli(&s(&["a", "b", "c", "d", "e"])),
            crate::verbs::exit::USAGE
        );
        assert_eq!(run_cli(&s(&[])), 2);
    }
}
