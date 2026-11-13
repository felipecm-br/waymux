//! Verbs for agents and shell scripts: `run`, `wait`, `events`, `report` and `api`.
//!
//! They are thin clients of what the daemon and tmux already do, with one contract: a target
//! names a pane the same way everywhere ([`resolve_in`]), and the exit status says what went
//! wrong ([`exit`]) so a script can branch on it without reading stderr.

use crate::adapters::AgentState;
use crate::rpc_client::{CallError, rpc_call};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Exit statuses shared by the verbs in this module. `run --wait` returns the command's own.
pub mod exit {
    pub const OK: i32 = 0;
    /// The command ran and failed: a wait timed out, the daemon refused a call.
    pub const FAILURE: i32 = 1;
    pub const USAGE: i32 = 2;
    /// The target names no pane, or more than one.
    pub const NOT_FOUND: i32 = 3;
    /// The daemon did not answer.
    pub const UNREACHABLE: i32 = 4;
}

const FIELD: char = '\x1f';

/// `10m`, `30s`, `2h`, `500ms`; a bare number is seconds.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let n: u64 = digits.parse().ok()?;
    match &text[digits.len()..] {
        "ms" => Some(Duration::from_millis(n)),
        "" | "s" => Some(Duration::from_secs(n)),
        "m" => n.checked_mul(60).map(Duration::from_secs),
        "h" => n.checked_mul(3600).map(Duration::from_secs),
        _ => None,
    }
}

/// One pane of the tmux server, as [`resolve_in`] sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub id: String,
    /// `session:window.pane`.
    pub address: String,
    pub window_name: String,
    /// The pane its window shows.
    pub active: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    Pane(String),
    NotFound,
    /// The panes the target could mean.
    Ambiguous(Vec<String>),
}

/// Finds the pane a target names: a pane id (`%12`), a tmux address (`main:2.1`, or `main:2`
/// for the window's active pane), then a window name, compared without case. A name that two
/// windows share is ambiguous rather than a guess.
pub fn resolve_in(rows: &[PaneRow], target: &str) -> Resolved {
    let target = target.trim();
    if target.is_empty() {
        return Resolved::NotFound;
    }
    if let Some(row) = rows.iter().find(|r| r.id == target || r.address == target) {
        return Resolved::Pane(row.id.clone());
    }
    let active = |pred: &dyn Fn(&PaneRow) -> bool| -> Vec<String> {
        rows.iter()
            .filter(|r| r.active && pred(r))
            .map(|r| r.id.clone())
            .collect()
    };
    let window = active(&|r| r.address.rsplit_once('.').is_some_and(|(w, _)| w == target));
    let mut found = if window.is_empty() {
        active(&|r| r.window_name.eq_ignore_ascii_case(target))
    } else {
        window
    };
    match found.len() {
        0 => Resolved::NotFound,
        1 => Resolved::Pane(found.remove(0)),
        _ => Resolved::Ambiguous(found),
    }
}

fn parse_pane_rows(out: &str) -> Vec<PaneRow> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split(FIELD);
            Some(PaneRow {
                id: f.next()?.to_string(),
                address: f.next()?.to_string(),
                window_name: f.next()?.to_string(),
                active: f.next()? == "1",
            })
        })
        .collect()
}

fn pane_rows() -> Option<Vec<PaneRow>> {
    let format = format!(
        "#{{pane_id}}{FIELD}#{{session_name}}:#{{window_index}}.#{{pane_index}}{FIELD}#{{window_name}}{FIELD}#{{pane_active}}"
    );
    crate::tmux_cmd::tmux_fields_out(&["list-panes", "-a", "-F", &format])
        .map(|out| parse_pane_rows(&out))
}

fn is_pane_id(text: &str) -> bool {
    text.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Resolves `target` against the tmux server. Without a server only a pane id can be taken, as
/// given: the daemon also tracks panes of agents that report from outside tmux.
fn resolve(target: &str) -> Resolved {
    match pane_rows() {
        Some(rows) => resolve_in(&rows, target),
        None if is_pane_id(target.trim()) => Resolved::Pane(target.trim().to_string()),
        None => Resolved::NotFound,
    }
}

/// The pane for `target`, or the exit status after saying why not.
fn pane_or_exit(command: &str, target: &str) -> Result<String, i32> {
    match resolve(target) {
        Resolved::Pane(id) => Ok(id),
        Resolved::NotFound => {
            eprintln!("waymux {command}: no pane matches '{target}'");
            Err(exit::NOT_FOUND)
        }
        Resolved::Ambiguous(panes) => {
            eprintln!(
                "waymux {command}: '{target}' matches {} panes ({}); name one by id",
                panes.len(),
                panes.join(", ")
            );
            Err(exit::NOT_FOUND)
        }
    }
}

fn unreachable(command: &str) -> i32 {
    eprintln!(
        "waymux {command}: the daemon is not running or unreachable on port {}",
        crate::rpc_client::daemon_port()
    );
    exit::UNREACHABLE
}

fn usage(spec: &str) -> i32 {
    eprintln!("Usage: waymux {spec}");
    exit::USAGE
}

/// Takes the value of a flag written `--flag value` or `--flag=value`: `None` when `arg` is
/// another flag, `Some(None)` when it is this one with its value missing.
#[allow(clippy::option_option)]
fn flag_value<'a>(
    arg: &'a str,
    name: &str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Option<Option<&'a str>> {
    if arg == name {
        return Some(rest.next().map(String::as_str));
    }
    arg.strip_prefix(name)
        .and_then(|v| v.strip_prefix('='))
        .map(Some)
}

// ───────────────────────────── wait ─────────────────────────────

const WAIT_USAGE: &str = "wait <pane> [--state <state>] [--for <duration>] [--json]";

/// `waymux wait`: blocks until the pane's agent reaches a state (default `idle`). Exit 0 when
/// it did, 1 when the time ran out or the pane closed first.
pub fn wait_cli(args: &[String]) -> i32 {
    let (mut target, mut state, mut limit, mut json) =
        (None, "idle", Duration::from_secs(300), false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--json" {
            json = true;
        } else if let Some(v) = flag_value(arg, "--state", &mut it) {
            match v {
                Some(v) if v.parse::<AgentState>().is_ok() => state = v,
                _ => return usage(WAIT_USAGE),
            }
        } else if let Some(v) = flag_value(arg, "--for", &mut it) {
            match v.and_then(parse_duration) {
                Some(d) => limit = d,
                None => return usage(WAIT_USAGE),
            }
        } else if target.is_none() && !arg.starts_with('-') {
            target = Some(arg.as_str());
        } else {
            return usage(WAIT_USAGE);
        }
    }
    let Some(target) = target else {
        return usage(WAIT_USAGE);
    };
    let pane = match pane_or_exit("wait", target) {
        Ok(pane) => pane,
        Err(code) => return code,
    };

    // The daemon counts whole seconds; the client gives it time to answer after the last one.
    let secs = limit.as_secs().max(1);
    let params =
        serde_json::json!({ "pane_id": pane, "target_state": state, "timeout_secs": secs });
    let result = match rpc_call(
        "agentState/wait",
        &params,
        secs.saturating_mul(1000).saturating_add(3000),
    ) {
        Ok(result) => result,
        Err(CallError::Unreachable) => return unreachable("wait"),
        Err(CallError::Rejected(reason)) => {
            eprintln!("waymux wait: {reason}");
            return exit::FAILURE;
        }
    };
    if json {
        println!("{result}");
    }
    match result["status"].as_str() {
        Some("reached") => exit::OK,
        Some("closed") => {
            eprintln!("waymux wait: {pane} closed before it was {state}");
            exit::FAILURE
        }
        _ => {
            let now = result["current_state"].as_str().unwrap_or("unknown");
            eprintln!("waymux wait: {pane} is still {now} after {secs}s");
            exit::FAILURE
        }
    }
}

// ───────────────────────────── goal ─────────────────────────────

const GOAL_USAGE: &str =
    "goal <pane> --check <command> [--max <rounds>] [--for <duration>] [--nudge <text>] [--json]";
/// Lines of the failing check's output the nudge quotes.
const GOAL_TAIL_LINES: usize = 40;
/// How long the agent has to start working on a nudge.
const GOAL_TAKE_SECS: u64 = 120;

/// The prompt that sends the agent back to work: the check, its status and the end of its output.
pub fn goal_nudge(
    check: &str,
    code: i32,
    output: &str,
    round: u32,
    max: u32,
    extra: Option<&str>,
) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let tail = lines[lines.len().saturating_sub(GOAL_TAIL_LINES)..].join("\n");
    let mut text = format!(
        "The goal is not met yet (check {round}/{max}): `{check}` exited {code}. Keep going until it passes; \
         do not stop to ask unless you are blocked."
    );
    if let Some(extra) = extra {
        text.push_str("\n\n");
        text.push_str(extra);
    }
    if !tail.trim().is_empty() {
        text.push_str("\n\nEnd of its output:\n```\n");
        text.push_str(&tail);
        text.push_str("\n```");
    }
    text
}

/// Waits for `state` on `pane`: `Ok(true)` reached, `Ok(false)` timed out, `Err` exit code.
fn wait_for(pane: &str, state: &str, secs: u64) -> Result<bool, i32> {
    let params =
        serde_json::json!({ "pane_id": pane, "target_state": state, "timeout_secs": secs.max(1) });
    match rpc_call(
        "agentState/wait",
        &params,
        secs.saturating_mul(1000).saturating_add(3000),
    ) {
        Ok(r) => match r["status"].as_str() {
            Some("reached") => Ok(true),
            Some("closed") => {
                eprintln!("waymux goal: {pane} closed");
                Err(exit::FAILURE)
            }
            _ => Ok(false),
        },
        Err(CallError::Unreachable) => Err(unreachable("goal")),
        Err(CallError::Rejected(reason)) => {
            eprintln!("waymux goal: {reason}");
            Err(exit::FAILURE)
        }
    }
}

/// `waymux goal`: `/goal` for any agent. Each time the pane's agent comes to rest, runs the
/// check in the pane's directory; while it fails, queues a nudge with the end of its output and
/// waits for the next rest. Exit 0 once the check passes, 1 after `--max` failed checks (5), a
/// round longer than `--for` (30m), or a nudge the agent did not take.
pub fn goal_cli(args: &[String]) -> i32 {
    let (mut target, mut check, mut max, mut limit, mut nudge, mut json) =
        (None, None, 5u32, Duration::from_secs(30 * 60), None, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--json" {
            json = true;
        } else if let Some(v) = flag_value(arg, "--check", &mut it) {
            match v {
                Some(v) if !v.trim().is_empty() => check = Some(v.to_string()),
                _ => return usage(GOAL_USAGE),
            }
        } else if let Some(v) = flag_value(arg, "--max", &mut it) {
            match v.and_then(|v| v.parse::<u32>().ok()).filter(|n| *n > 0) {
                Some(n) => max = n,
                None => return usage(GOAL_USAGE),
            }
        } else if let Some(v) = flag_value(arg, "--for", &mut it) {
            match v.and_then(parse_duration) {
                Some(d) => limit = d,
                None => return usage(GOAL_USAGE),
            }
        } else if let Some(v) = flag_value(arg, "--nudge", &mut it) {
            match v {
                Some(v) => nudge = Some(v.to_string()),
                None => return usage(GOAL_USAGE),
            }
        } else if target.is_none() && !arg.starts_with('-') {
            target = Some(arg.as_str());
        } else {
            return usage(GOAL_USAGE);
        }
    }
    let (Some(target), Some(check)) = (target, check) else {
        return usage(GOAL_USAGE);
    };
    let pane = match pane_or_exit("goal", target) {
        Ok(pane) => pane,
        Err(code) => return code,
    };
    let dir = crate::checkpoint::pane_dir(&pane).unwrap_or_else(|| PathBuf::from("."));
    let finish = |status: &str, rounds: u32, code: i32| {
        if json {
            println!(
                "{}",
                serde_json::json!({ "pane_id": pane, "status": status, "checks": rounds })
            );
        }
        code
    };
    for round in 1..=max {
        match wait_for(&pane, "idle", limit.as_secs()) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("waymux goal: {pane} still busy after {}s", limit.as_secs());
                return finish("timeout", round - 1, exit::FAILURE);
            }
            Err(code) => return finish("error", round - 1, code),
        }
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{check} 2>&1"))
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .output();
        let (code, output) = match out {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).to_string(),
            ),
            Err(e) => (-1, e.to_string()),
        };
        if code == 0 {
            if !json {
                println!("󰄬 Goal met on {pane}: `{check}` passed (check {round})");
            }
            return finish("met", round, exit::OK);
        }
        if round == max {
            eprintln!("waymux goal: `{check}` still fails (exit {code}) after {max} checks");
            return finish("gave_up", round, exit::FAILURE);
        }
        let text = goal_nudge(&check, code, &output, round, max, nudge.as_deref());
        let sent = crate::delivery::dispatch_prompt(&pane, &text, "goal");
        if !json {
            println!(
                "󰇮 Check {round}/{max} failed (exit {code}): {}",
                sent.message(&pane)
            );
        }
        if matches!(sent, crate::delivery::Dispatch::Refused(_)) {
            return finish("refused", round, exit::FAILURE);
        }
        match wait_for(&pane, "working", GOAL_TAKE_SECS) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "waymux goal: {pane} did not take the nudge in {GOAL_TAKE_SECS}s (waymux queue ls)"
                );
                return finish("not_taken", round, exit::FAILURE);
            }
            Err(code) => return finish("error", round, code),
        }
    }
    finish("gave_up", max, exit::FAILURE)
}

// ───────────────────────────── events ─────────────────────────────

const EVENTS_USAGE: &str =
    "events [--json] [--pane <pane>] [--for <duration>] [--interval <duration>]";

/// A pane's state as `agentState/list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub state: String,
    pub message: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub pane_id: String,
    pub state: String,
    /// None for a pane first seen.
    pub previous: Option<String>,
    pub message: Option<String>,
}

/// What changed between two snapshots, in pane order. A pane that left the list is `closed`.
pub fn diff_states(prev: &BTreeMap<String, Seen>, next: &BTreeMap<String, Seen>) -> Vec<Event> {
    let mut events = Vec::new();
    for (pane, seen) in next {
        let before = prev.get(pane);
        if before.is_none_or(|b| b.state != seen.state) {
            events.push(Event {
                pane_id: pane.clone(),
                state: seen.state.clone(),
                previous: before.map(|b| b.state.clone()),
                message: seen.message.clone(),
            });
        }
    }
    for (pane, seen) in prev {
        if !next.contains_key(pane) && seen.state != "closed" {
            events.push(Event {
                pane_id: pane.clone(),
                state: "closed".into(),
                previous: Some(seen.state.clone()),
                message: None,
            });
        }
    }
    events.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    events
}

fn parse_states(result: &serde_json::Value) -> BTreeMap<String, Seen> {
    let mut states = BTreeMap::new();
    for (pane, info) in result.as_object().into_iter().flatten() {
        if let Some(state) = info["state"].as_str().filter(|s| !s.is_empty()) {
            let message = info["message"].as_str().map(str::to_string);
            states.insert(
                pane.clone(),
                Seen {
                    state: state.to_string(),
                    message,
                },
            );
        }
    }
    states
}

fn event_line(event: &Event, ts: u64, json: bool) -> String {
    if json {
        serde_json::json!({
            "ts": ts,
            "pane_id": event.pane_id,
            "state": event.state,
            "previous": event.previous,
            "message": event.message,
        })
        .to_string()
    } else {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        format!(
            "{ts}\t{}\t{}\t{}\t{}",
            event.pane_id,
            event.state,
            event.previous.as_deref().unwrap_or("-"),
            event.message.as_deref().map(clean).unwrap_or_default()
        )
    }
}

/// `waymux events`: one line per state change (TSV, or NDJSON with `--json`), starting with
/// the state every pane is in now. It reads the daemon's list on an interval, so two changes
/// inside one interval show as one. Runs until interrupted or `--for` passes; exit 4 when the
/// daemon stops answering.
pub fn events_cli(args: &[String]) -> i32 {
    let (mut json, mut only, mut limit, mut interval) =
        (false, None, None, Duration::from_millis(250));
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--json" {
            json = true;
        } else if let Some(v) = flag_value(arg, "--pane", &mut it) {
            let Some(v) = v else {
                return usage(EVENTS_USAGE);
            };
            match pane_or_exit("events", v) {
                Ok(pane) => only = Some(pane),
                Err(code) => return code,
            }
        } else if let Some(v) = flag_value(arg, "--for", &mut it) {
            match v.and_then(parse_duration) {
                Some(d) => limit = Some(d),
                None => return usage(EVENTS_USAGE),
            }
        } else if let Some(v) = flag_value(arg, "--interval", &mut it) {
            match v.and_then(parse_duration) {
                Some(d) => interval = d.max(Duration::from_millis(20)),
                None => return usage(EVENTS_USAGE),
            }
        } else {
            return usage(EVENTS_USAGE);
        }
    }

    let started = Instant::now();
    let mut prev = BTreeMap::new();
    let stdout = std::io::stdout();
    loop {
        let Ok(result) = rpc_call("agentState/list", &serde_json::Value::Null, 1000) else {
            return unreachable("events");
        };
        let mut next = parse_states(&result);
        if let Some(pane) = &only {
            next.retain(|id, _| id == pane);
        }
        let ts = crate::sys::unix_ms();
        let mut out = stdout.lock();
        for event in diff_states(&prev, &next) {
            if writeln!(out, "{}", event_line(&event, ts, json)).is_err() {
                return exit::OK;
            }
        }
        if out.flush().is_err() {
            return exit::OK;
        }
        drop(out);
        prev = next;
        if limit.is_some_and(|l| started.elapsed() >= l) {
            return exit::OK;
        }
        std::thread::sleep(interval);
    }
}

// ───────────────────────────── report ─────────────────────────────

const REPORT_USAGE: &str = "report <state> [--pane <pane>] [--msg <text>] [--progress <0-100>]";

/// The message a report carries: the progress first, so a narrow column still shows it.
fn report_message(progress: Option<u8>, msg: Option<&str>) -> Option<String> {
    match (progress, msg.map(str::trim).filter(|m| !m.is_empty())) {
        (Some(p), Some(m)) => Some(format!("{p}% {m}")),
        (Some(p), None) => Some(format!("{p}%")),
        (None, m) => m.map(str::to_string),
    }
}

/// `waymux report`: lets any script show up like an agent (`working`, `idle`, `error`, …) in
/// the status bar, the Cockpit and `wait`. The pane defaults to `$TMUX_PANE`.
pub fn report_cli(args: &[String]) -> i32 {
    let (mut state, mut target, mut msg, mut progress) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(v) = flag_value(arg, "--pane", &mut it) {
            target = v;
        } else if let Some(v) = flag_value(arg, "--msg", &mut it) {
            msg = v;
        } else if let Some(v) = flag_value(arg, "--progress", &mut it) {
            match v.and_then(|v| v.parse::<u8>().ok()).filter(|p| *p <= 100) {
                Some(p) => progress = Some(p),
                None => return usage(REPORT_USAGE),
            }
        } else if state.is_none() && !arg.starts_with('-') {
            state = Some(arg.as_str());
        } else {
            return usage(REPORT_USAGE);
        }
    }
    let Some(state) = state else {
        return usage(REPORT_USAGE);
    };
    if let Err(reason) = state.parse::<AgentState>() {
        eprintln!("waymux report: {reason}");
        return exit::USAGE;
    }
    let own = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty());
    let pane = if let Some(target) = target {
        match pane_or_exit("report", target) {
            Ok(pane) => pane,
            Err(code) => return code,
        }
    } else if let Some(pane) = own {
        pane
    } else {
        eprintln!("waymux report: not in a tmux pane; name one with --pane");
        return exit::USAGE;
    };

    let payload = serde_json::json!({
        "pane_id": pane,
        "state": state,
        "message": report_message(progress, msg),
        "timestamp": crate::sys::unix_ms(),
    });
    let token = crate::rpc_client::read_token();
    match crate::rpc_client::http_post_json(
        crate::rpc_client::daemon_port(),
        "/api/status",
        token.as_deref(),
        &payload,
        1000,
    ) {
        None => unreachable("report"),
        Some((code, _)) if (200..300).contains(&code) => exit::OK,
        Some((code, body)) => {
            eprintln!(
                "waymux report: the daemon answered HTTP {code}: {}",
                body.trim()
            );
            exit::FAILURE
        }
    }
}

// ───────────────────────────── api ─────────────────────────────

const API_USAGE: &str =
    "api <list [--json] | describe <method> [--json] | call <method> [<json> | @<file> | -]>";

pub struct MethodSpec {
    pub name: &'static str,
    /// `name` for a required parameter, `name?` for an optional one.
    pub params: &'static [&'static str],
    pub about: &'static str,
}

const fn method(
    name: &'static str,
    params: &'static [&'static str],
    about: &'static str,
) -> MethodSpec {
    MethodSpec {
        name,
        params,
        about,
    }
}

/// Every method `/rpc` serves. A unit test keeps it equal to the arms of `api::rpc::call`.
pub const METHODS: &[MethodSpec] = &[
    method(
        "agentState/update",
        &["pane_id", "state", "timestamp?"],
        "Set a pane's agent state",
    ),
    method(
        "agentState/list",
        &[],
        "Every pane's state, message and timestamps",
    ),
    method(
        "agentState/dismiss",
        &["pane_id?", "target?"],
        "Clear a pane's attention badge",
    ),
    method(
        "agentState/wait",
        &["pane_id", "target_state?", "timeout_secs?"],
        "Block until a pane reaches a state (default idle, 300 s)",
    ),
    method(
        "agent/recap",
        &["pane_id?"],
        "What an agent did while you were away",
    ),
    method(
        "queue/push",
        &["pane_id", "text", "by?"],
        "Hold a prompt until the agent is at rest, then deliver it",
    ),
    method("queue/list", &["pane_id?"], "Prompts waiting for delivery"),
    method(
        "queue/cancel",
        &["pane_id", "id?"],
        "Drop one queued prompt, or all of a pane's",
    ),
    method(
        "queue/edit",
        &["pane_id", "id", "text"],
        "Replace the text of a prompt not typed yet",
    ),
    method(
        "queue/send",
        &["pane_id", "id?"],
        "Type a queued prompt now, over a draft hold (next, when the agent is busy)",
    ),
    method(
        "approval/hold",
        &["pane_id", "tool", "line"],
        "Hold a permission prompt for an answer from the Cockpit or phone",
    ),
    method("approval/get", &["pane_id"], "The prompt held for a pane"),
    method(
        "approval/answer",
        &["pane_id", "id", "decision", "line", "risk_ack?"],
        "Answer a held prompt",
    ),
    method(
        "hook/token",
        &["pane_id"],
        "Mint a token limited to one pane's hooks",
    ),
    method("phone/snapshot", &[], "The phone page's list of agents"),
    method(
        "phone/diff",
        &["pane_id"],
        "The diff of an agent's last turn",
    ),
    method(
        "phone/prompt",
        &["pane_id", "text"],
        "Queue a prompt typed on the phone",
    ),
    method(
        "phone/token",
        &["rotate?", "prompt?"],
        "The phone surface's token",
    ),
    method("phone/alert", &[], "Send a test alert to the ntfy topic"),
    method(
        "tmux.split_pane",
        &["command?", "target_pane?", "vertical?"],
        "Split a pane",
    ),
    method(
        "tmux.new_window",
        &["name?", "target?", "command?"],
        "Create a window",
    ),
    method(
        "tmux.new_session",
        &["name", "directory?", "command?"],
        "Create a detached session",
    ),
    method("tmux.kill_pane", &["target"], "Kill a pane"),
    method("tmux.kill_window", &["target"], "Kill a window"),
    method("tmux.kill_session", &["target"], "Kill a session"),
    method(
        "tmux.capture_pane",
        &[
            "target?",
            "target_pane?",
            "start_line?",
            "end_line?",
            "escape_sequences?",
        ],
        "Read a pane's screen and scrollback",
    ),
    method("tmux.list_panes", &["target?", "all?"], "List panes"),
    method(
        "tmux.list_windows",
        &["target?", "target_session?"],
        "List windows",
    ),
    method("tmux.list_sessions", &[], "List sessions"),
    method(
        "tmux.send_keys",
        &["keys", "target?", "target_pane?", "literal?"],
        "Type keys into a pane (queue/push is the safe way to prompt an agent)",
    ),
    method("tmux.select_window", &["target?"], "Focus a window"),
    method("tmux.select_pane", &["target?"], "Focus a pane"),
    method(
        "tmux.display_message",
        &["message", "target?"],
        "Show a message in the status line",
    ),
    method("tmux.ring_bell", &["target?"], "Ring a pane's bell"),
    method(
        "tmux.select_layout",
        &["layout", "target?"],
        "Apply a layout to a window",
    ),
];

fn method_json(m: &MethodSpec) -> serde_json::Value {
    let params: Vec<_> = m
        .params
        .iter()
        .map(|p| {
            let name = p.trim_end_matches('?');
            serde_json::json!({ "name": name, "required": !p.ends_with('?') })
        })
        .collect();
    serde_json::json!({ "method": m.name, "about": m.about, "params": params })
}

fn print_json(value: &serde_json::Value) {
    if std::io::stdout().is_terminal() {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        println!("{value}");
    }
}

/// The params of `api call`: a JSON value, `@file`, or `-` for stdin; nothing means none.
fn read_params(arg: Option<&str>) -> Result<serde_json::Value, String> {
    let text = match arg {
        None => return Ok(serde_json::Value::Null),
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("stdin: {e}"))?;
            text
        }
        Some(arg) => match arg.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
            None => arg.to_string(),
        },
    };
    serde_json::from_str(&text).map_err(|e| format!("params are not JSON: {e}"))
}

/// `waymux api`: what `/rpc` serves (`list`, `describe`, both answered without the daemon) and
/// a way to call it without writing the HTTP request (`call`).
pub fn api_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    match words.as_slice() {
        ["list"] => {
            if json {
                print_json(&METHODS.iter().map(method_json).collect());
            } else {
                for m in METHODS {
                    println!("{}\t{}", m.name, m.about);
                }
            }
            exit::OK
        }
        ["describe", name] => {
            let Some(m) = METHODS.iter().find(|m| m.name == *name) else {
                eprintln!("waymux api: no method '{name}'; see 'waymux api list'");
                return exit::NOT_FOUND;
            };
            if json {
                print_json(&method_json(m));
            } else {
                println!("{}\n  {}", m.name, m.about);
                if m.params.is_empty() {
                    println!("  params: none");
                }
                for p in m.params {
                    match p.strip_suffix('?') {
                        Some(name) => println!("  {name:<18} optional"),
                        None => println!("  {p:<18} required"),
                    }
                }
            }
            exit::OK
        }
        ["call", name, rest @ ..] if rest.len() <= 1 => {
            let params = match read_params(rest.first().copied()) {
                Ok(params) => params,
                Err(reason) => {
                    eprintln!("waymux api: {reason}");
                    return exit::USAGE;
                }
            };
            // A wait may hold the call for as long as it was asked to.
            let hold = params["timeout_secs"].as_u64().unwrap_or(0);
            match rpc_call(
                name,
                &params,
                hold.saturating_mul(1000).saturating_add(15_000),
            ) {
                Ok(result) => {
                    print_json(&result);
                    exit::OK
                }
                Err(CallError::Unreachable) => unreachable("api"),
                Err(CallError::Rejected(reason)) => {
                    eprintln!("waymux api: {reason}");
                    if reason.to_ascii_lowercase().contains("method not found") {
                        exit::NOT_FOUND
                    } else {
                        exit::FAILURE
                    }
                }
            }
        }
        _ => usage(API_USAGE),
    }
}

// ───────────────────────────── run ─────────────────────────────

const RUN_USAGE: &str = "run [--window | --split <right|left|below|above>] [--ratio <percent>] [-t <pane>] [--cwd <dir>] [--name <label>] [--focus] [--keep-open] [--wait [--for <duration>] [--capture]] [--json] [--] <command>...";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Placement {
    Window,
    Right,
    Left,
    Below,
    Above,
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one per command-line switch
pub struct RunOpts {
    pub placement: Placement,
    pub ratio: Option<u8>,
    pub target: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub focus: bool,
    pub keep_open: bool,
    pub wait: bool,
    pub limit: Option<Duration>,
    pub capture: bool,
    pub json: bool,
    pub command: Vec<String>,
}

pub fn parse_run_args(args: &[String]) -> Option<RunOpts> {
    let mut o = RunOpts {
        placement: Placement::Window,
        ratio: None,
        target: None,
        cwd: None,
        name: None,
        focus: false,
        keep_open: false,
        wait: false,
        limit: None,
        capture: false,
        json: false,
        command: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            o.command.extend(it.by_ref().cloned());
        } else if !arg.starts_with('-') {
            o.command.push(arg.clone());
            o.command.extend(it.by_ref().cloned());
        } else if arg == "--window" {
            o.placement = Placement::Window;
        } else if arg == "--focus" {
            o.focus = true;
        } else if arg == "--keep-open" {
            o.keep_open = true;
        } else if arg == "--wait" {
            o.wait = true;
        } else if arg == "--capture" {
            o.capture = true;
        } else if arg == "--json" {
            o.json = true;
        } else if let Some(v) = flag_value(arg, "--split", &mut it) {
            o.placement = match v? {
                "right" => Placement::Right,
                "left" => Placement::Left,
                "below" | "down" => Placement::Below,
                "above" | "up" => Placement::Above,
                _ => return None,
            };
        } else if let Some(v) = flag_value(arg, "--ratio", &mut it) {
            o.ratio = Some(v?.parse::<u8>().ok().filter(|p| (1..=99).contains(p))?);
        } else if let Some(v) = flag_value(arg, "-t", &mut it) {
            o.target = Some(v?.to_string());
        } else if let Some(v) = flag_value(arg, "--cwd", &mut it) {
            o.cwd = Some(v?.to_string());
        } else if let Some(v) = flag_value(arg, "--name", &mut it) {
            o.name = Some(v?.to_string());
        } else {
            // The last flag known: anything else is not a `run` flag.
            let v = flag_value(arg, "--for", &mut it)?;
            o.limit = Some(parse_duration(v?)?);
        }
    }
    // Capturing and a time limit only mean something while waiting.
    let waits = o.wait || (!o.capture && o.limit.is_none());
    (!o.command.is_empty() && waits).then_some(o)
}

/// Files a waited run reports through: its exit status, and its screen when captured.
#[derive(Debug, Clone)]
pub struct RunFiles {
    pub status: PathBuf,
    pub screen: Option<PathBuf>,
}

/// The `/bin/sh` script the new pane runs (the user's own shell may not be POSIX). One word is a shell snippet (`'make && make test'`);
/// several are a command and its arguments, each passed as written. A waited run then saves
/// its screen (when asked) and its exit status, the status last and by rename, so a reader
/// never sees half of it.
pub fn build_script(command: &[String], files: Option<&RunFiles>, keep_open: bool) -> String {
    use crate::sys::shell_quote;
    let mut script = match command {
        [snippet] => snippet.clone(),
        words => words
            .iter()
            .map(|w| shell_quote(w))
            .collect::<Vec<_>>()
            .join(" "),
    };
    script.push_str("\ns=$?\n");
    if let Some(files) = files {
        if let Some(screen) = &files.screen {
            let screen = shell_quote(&screen.to_string_lossy());
            let _ = writeln!(
                script,
                "tmux capture-pane -p -J -S -200 -t \"$TMUX_PANE\" > {screen} 2>/dev/null"
            );
        }
        let status = files.status.to_string_lossy();
        let (tmp, done) = (shell_quote(&format!("{status}.tmp")), shell_quote(&status));
        let _ = writeln!(script, "printf %s \"$s\" > {tmp} && mv {tmp} {done}");
    }
    script.push_str(if keep_open {
        "exec \"${SHELL:-sh}\"\n"
    } else {
        "exit \"$s\"\n"
    });
    script
}

/// The tmux arguments that open the pane and print its id. `target` is the pane to split, or
/// the window to open the new one after.
pub fn tmux_args(o: &RunOpts, target: Option<&str>, script: &str) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |words: &[&str]| a.extend(words.iter().map(ToString::to_string));
    match o.placement {
        Placement::Window => push(&["new-window"]),
        Placement::Right => push(&["split-window", "-h"]),
        Placement::Left => push(&["split-window", "-h", "-b"]),
        Placement::Below => push(&["split-window", "-v"]),
        Placement::Above => push(&["split-window", "-v", "-b"]),
    }
    if !o.focus {
        push(&["-d"]);
    }
    push(&["-P", "-F", "#{pane_id}"]);
    if o.placement == Placement::Window {
        if let Some(name) = &o.name {
            push(&["-n", name]);
        }
        // Next to the window it was started from, not at the end of the session.
        if let Some(target) = target {
            push(&["-a", "-t", target]);
        }
    } else {
        if let Some(ratio) = o.ratio {
            push(&["-l", &format!("{ratio}%")]);
        }
        if let Some(target) = target {
            push(&["-t", target]);
        }
    }
    if let Some(cwd) = &o.cwd {
        push(&["-c", cwd]);
    }
    push(&[&format!("/bin/sh -c {}", crate::sys::shell_quote(script))]);
    a
}

fn run_files(capture: bool) -> RunFiles {
    let dir = crate::presets::private_runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("run");
    let _ = std::fs::create_dir_all(&dir);
    let stem = format!("{}-{}", std::process::id(), crate::sys::unix_ms());
    RunFiles {
        status: dir.join(format!("{stem}.status")),
        screen: capture.then(|| dir.join(format!("{stem}.screen"))),
    }
}

fn pane_alive(pane: &str) -> bool {
    crate::tmux_cmd::tmux_out(&["display-message", "-p", "-t", pane, "#{pane_id}"])
        .is_some_and(|id| id == pane)
}

/// Why a waited run ended without an exit status.
enum Lost {
    TimedOut,
    PaneClosed,
}

fn await_status(status: &Path, pane: &str, limit: Option<Duration>) -> Result<i32, Lost> {
    let started = Instant::now();
    let mut checked = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(status) {
            return Ok(text.trim().parse().unwrap_or(exit::FAILURE));
        }
        if limit.is_some_and(|l| started.elapsed() >= l) {
            return Err(Lost::TimedOut);
        }
        if checked.elapsed() >= Duration::from_millis(500) {
            checked = Instant::now();
            // The status is written before the pane goes, so look once more before giving up.
            if !pane_alive(pane) && !status.exists() {
                return Err(Lost::PaneClosed);
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// `waymux run`: starts a command in a new window (or a split of the caller's pane) without
/// taking the focus, and prints the new pane's id. With `--wait` it blocks until the command
/// ends and exits with the command's own status.
pub fn run_cli(args: &[String]) -> i32 {
    let Some(opts) = parse_run_args(args) else {
        return usage(RUN_USAGE);
    };
    // Without a target tmux would pick the pane the human is looking at, not the caller's.
    let own = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty());
    let target = match &opts.target {
        Some(target) => match pane_or_exit("run", target) {
            Ok(pane) => Some(pane),
            Err(code) => return code,
        },
        None => own,
    };

    // `new-window -t` takes a window, so the pane's own is looked up.
    let target = match (opts.placement, target) {
        (Placement::Window, Some(pane)) => {
            crate::tmux_cmd::tmux_out(&["display-message", "-p", "-t", &pane, "#{window_id}"])
                .filter(|w| w.starts_with('@'))
        }
        (_, target) => target,
    };

    let files = opts.wait.then(|| run_files(opts.capture));
    let script = build_script(&opts.command, files.as_ref(), opts.keep_open);
    let words = tmux_args(&opts, target.as_deref(), &script);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some(pane) = crate::tmux_cmd::tmux_out(&words).filter(|p| is_pane_id(p)) else {
        eprintln!("waymux run: tmux could not open the pane (is a tmux server running?)");
        return exit::FAILURE;
    };
    if opts.placement != Placement::Window
        && let Some(name) = &opts.name
    {
        let _ = crate::tmux_cmd::tmux_status(&["select-pane", "-t", &pane, "-T", name]);
    }

    let Some(files) = files else {
        if opts.json {
            println!("{}", serde_json::json!({ "pane_id": pane }));
        } else {
            println!("{pane}");
        }
        return exit::OK;
    };

    let outcome = await_status(&files.status, &pane, opts.limit);
    let screen = files
        .screen
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim_end().to_string());
    let _ = std::fs::remove_file(&files.status);
    if let Some(path) = &files.screen {
        let _ = std::fs::remove_file(path);
    }
    let code = match outcome {
        Ok(code) => code,
        Err(Lost::TimedOut) => {
            eprintln!("waymux run: still running in {pane} after the time given");
            exit::FAILURE
        }
        Err(Lost::PaneClosed) => {
            eprintln!("waymux run: {pane} closed before the command finished");
            exit::FAILURE
        }
    };
    if opts.json {
        println!(
            "{}",
            serde_json::json!({
                "pane_id": pane,
                "exit_status": outcome.is_ok().then_some(code),
                "output": screen,
            })
        );
    } else if let Some(screen) = screen.filter(|s| !s.is_empty()) {
        println!("{screen}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    fn row(id: &str, address: &str, name: &str, active: bool) -> PaneRow {
        PaneRow {
            id: id.into(),
            address: address.into(),
            window_name: name.into(),
            active,
        }
    }

    #[test]
    fn test_parse_duration_units_and_rejects() {
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("45"), Some(Duration::from_secs(45)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        for bad in ["", "m", "1d", "-5s", "1.5m", "ten", "99999999999999999999h"] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn test_resolve_prefers_id_then_address_then_window_name() {
        let rows = [
            row("%1", "main:1.0", "api", true),
            row("%2", "main:1.1", "api", false),
            row("%3", "main:2.0", "Tests", true),
            row("%4", "work:1.0", "docs", true),
            row("%5", "work:2.0", "docs", true),
        ];
        assert_eq!(resolve_in(&rows, "%2"), Resolved::Pane("%2".into()));
        assert_eq!(resolve_in(&rows, "main:1.1"), Resolved::Pane("%2".into()));
        // A window address or name means the pane that window shows.
        assert_eq!(resolve_in(&rows, "main:1"), Resolved::Pane("%1".into()));
        assert_eq!(resolve_in(&rows, "api"), Resolved::Pane("%1".into()));
        assert_eq!(resolve_in(&rows, "tests"), Resolved::Pane("%3".into()));
        assert_eq!(
            resolve_in(&rows, "docs"),
            Resolved::Ambiguous(vec!["%4".into(), "%5".into()])
        );
        for missing in ["%9", "nope", "", "  ", "main:9"] {
            assert_eq!(
                resolve_in(&rows, missing),
                Resolved::NotFound,
                "{missing:?}"
            );
        }
    }

    #[test]
    fn test_pane_rows_parse_and_skip_short_lines() {
        let out = "%1\x1fmain:1.0\x1fapi\x1f1\n%2\x1fmain:1.1\x1fapi\x1f0\nbroken\n";
        assert_eq!(
            parse_pane_rows(out),
            vec![
                row("%1", "main:1.0", "api", true),
                row("%2", "main:1.1", "api", false)
            ]
        );
        assert!(is_pane_id("%12") && !is_pane_id("%") && !is_pane_id("12") && !is_pane_id("%1a"));
    }

    fn seen(pairs: &[(&str, &str)]) -> BTreeMap<String, Seen> {
        pairs
            .iter()
            .map(|(pane, state)| {
                (
                    (*pane).to_string(),
                    Seen {
                        state: (*state).to_string(),
                        message: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn test_diff_states_reports_new_changed_and_closed_panes_only() {
        let before = seen(&[("%1", "working"), ("%2", "idle"), ("%3", "permission")]);
        let after = seen(&[("%1", "idle"), ("%2", "idle"), ("%4", "working")]);
        let events = diff_states(&before, &after);
        let brief: Vec<_> = events
            .iter()
            .map(|e| (e.pane_id.as_str(), e.state.as_str(), e.previous.as_deref()))
            .collect();
        assert_eq!(
            brief,
            vec![
                ("%1", "idle", Some("working")),
                ("%3", "closed", Some("permission")),
                ("%4", "working", None),
            ]
        );
        assert_eq!(diff_states(&after, &after), vec![]);
        // The first snapshot is every pane, with no previous state.
        assert_eq!(diff_states(&BTreeMap::new(), &before).len(), 3);
    }

    #[test]
    fn test_event_lines_are_one_line_each() {
        let event = Event {
            pane_id: "%1".into(),
            state: "permission".into(),
            previous: Some("working".into()),
            message: Some("Bash:\tcargo\ntest".into()),
        };
        assert_eq!(
            event_line(&event, 7, false),
            "7\t%1\tpermission\tworking\tBash: cargo test"
        );
        let json: serde_json::Value = serde_json::from_str(&event_line(&event, 7, true)).unwrap();
        assert_eq!(json["previous"], "working");
        assert_eq!(json["message"], "Bash:\tcargo\ntest");
        assert!(!event_line(&event, 7, true).contains('\n'));

        let states = parse_states(&serde_json::json!({
            "%1": { "state": "working", "message": "cargo test" },
            "%2": { "state": "" },
            "%3": "junk",
        }));
        assert_eq!(states.len(), 1);
        assert_eq!(states["%1"].message.as_deref(), Some("cargo test"));
    }

    #[test]
    fn test_report_message_puts_progress_first() {
        assert_eq!(
            report_message(Some(40), Some("tests")).as_deref(),
            Some("40% tests")
        );
        assert_eq!(report_message(Some(100), None).as_deref(), Some("100%"));
        assert_eq!(report_message(None, Some("  ")), None);
        assert_eq!(
            report_message(None, Some("build")).as_deref(),
            Some("build")
        );
    }

    #[test]
    fn test_goal_nudge_quotes_the_check_and_the_tail_only() {
        let out = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = goal_nudge(
            "cargo test",
            101,
            &out,
            2,
            5,
            Some("Fix the flaky test first."),
        );
        assert!(text.starts_with("The goal is not met yet (check 2/5): `cargo test` exited 101."));
        assert!(text.contains("Fix the flaky test first."));
        assert!(
            text.contains("line 100") && text.contains("line 61") && !text.contains("line 60\n")
        );
        assert!(!goal_nudge("make", 1, "  \n", 1, 1, None).contains("End of its output"));
    }

    #[test]
    fn test_goal_rejects_bad_arguments_before_any_call() {
        assert_eq!(
            goal_cli(&strings(&["%1"])),
            exit::USAGE,
            "--check is required"
        );
        assert_eq!(goal_cli(&strings(&["%1", "--check", " "])), exit::USAGE);
        assert_eq!(
            goal_cli(&strings(&["%1", "--check", "true", "--max", "0"])),
            exit::USAGE
        );
        assert_eq!(
            goal_cli(&strings(&["%1", "--check", "true", "--for", "soon"])),
            exit::USAGE
        );
        assert_eq!(goal_cli(&strings(&["--check", "true"])), exit::USAGE);
    }

    #[test]
    fn test_report_and_wait_reject_bad_arguments_before_any_call() {
        assert_eq!(report_cli(&[]), exit::USAGE);
        assert_eq!(report_cli(&strings(&["thinking"])), exit::USAGE);
        assert_eq!(
            report_cli(&strings(&["working", "--progress", "101"])),
            exit::USAGE
        );
        assert_eq!(wait_cli(&[]), exit::USAGE);
        assert_eq!(wait_cli(&strings(&["%1", "--for", "soon"])), exit::USAGE);
        assert_eq!(
            wait_cli(&strings(&["%1", "--state", "thinking"])),
            exit::USAGE
        );
        assert_eq!(events_cli(&strings(&["--nope"])), exit::USAGE);
        assert_eq!(api_cli(&[]), exit::USAGE);
        assert_eq!(api_cli(&strings(&["describe", "no/such"])), exit::NOT_FOUND);
        assert_eq!(
            api_cli(&strings(&["call", "agentState/list", "{not json"])),
            exit::USAGE
        );
        assert_eq!(run_cli(&[]), exit::USAGE);
    }

    /// The method names in the arms of `api::rpc::call`, read from its source.
    fn served_methods() -> Vec<String> {
        let source = include_str!("api/rpc.rs");
        let body = source
            .split_once("async fn call(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .unwrap();
        let mut names = Vec::new();
        for line in body.lines() {
            let line = line.trim_start();
            if let Some((arm, _)) = line.split_once("=>")
                && arm.starts_with('"')
                && let Some(name) = arm.split('"').nth(1)
            {
                names.push(name.to_string());
            }
        }
        names
    }

    #[test]
    fn test_method_table_matches_the_rpc_dispatcher() {
        let mut served = served_methods();
        served.sort();
        let mut listed: Vec<String> = METHODS.iter().map(|m| m.name.to_string()).collect();
        listed.sort();
        assert!(
            served.len() > 20,
            "the dispatcher was not parsed: {served:?}"
        );
        assert_eq!(listed, served, "update METHODS in src/verbs.rs");
        for m in METHODS {
            assert_ne!(m.about, "");
            assert_eq!(
                method_json(m)["params"].as_array().unwrap().len(),
                m.params.len()
            );
        }
        assert_eq!(method_json(&METHODS[0])["params"][2]["required"], false);
    }

    #[test]
    fn test_read_params_takes_json_a_file_or_nothing() {
        assert_eq!(read_params(None).unwrap(), serde_json::Value::Null);
        assert_eq!(read_params(Some(r#"{"a":1}"#)).unwrap()["a"], 1);
        assert!(read_params(Some("{")).is_err());
        let path = std::env::temp_dir().join(format!("waymux-params-{}", std::process::id()));
        std::fs::write(&path, r#"{"pane_id":"%1"}"#).unwrap();
        let arg = format!("@{}", path.display());
        assert_eq!(read_params(Some(&arg)).unwrap()["pane_id"], "%1");
        std::fs::remove_file(&path).unwrap();
        assert!(read_params(Some(&arg)).is_err());
    }

    #[test]
    fn test_parse_run_args_splits_flags_from_the_command() {
        let o = parse_run_args(&strings(&[
            "--split",
            "below",
            "--ratio=30",
            "-t",
            "%4",
            "--wait",
            "--for",
            "10m",
            "--",
            "cargo",
            "test",
            "--wait",
        ]))
        .unwrap();
        assert_eq!(o.placement, Placement::Below);
        assert_eq!(o.ratio, Some(30));
        assert_eq!(o.target.as_deref(), Some("%4"));
        assert!(o.wait && !o.focus && !o.keep_open);
        assert_eq!(o.limit, Some(Duration::from_secs(600)));
        // Flags after the command belong to the command.
        assert_eq!(o.command, strings(&["cargo", "test", "--wait"]));

        // The command may start without `--`.
        let o = parse_run_args(&strings(&["--name", "tests", "make", "-j4"])).unwrap();
        assert_eq!(o.placement, Placement::Window);
        assert_eq!(o.command, strings(&["make", "-j4"]));

        for bad in [
            vec!["--wait"],
            vec!["--split", "diagonal", "true"],
            vec!["--ratio", "0", "true"],
            vec!["--ratio", "100", "true"],
            vec!["--bogus", "true"],
            vec!["--capture", "true"],
            vec!["--for", "5s", "true"],
            vec!["--cwd"],
        ] {
            assert_eq!(parse_run_args(&strings(&bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn test_build_script_quotes_words_and_keeps_a_snippet() {
        let files = RunFiles {
            status: PathBuf::from("/run/it's/1.status"),
            screen: Some(PathBuf::from("/run/1.screen")),
        };
        let script = build_script(&strings(&["echo", "a b", "it's"]), Some(&files), false);
        assert!(
            script.starts_with("'echo' 'a b' 'it'\\''s'\ns=$?\n"),
            "{script}"
        );
        assert!(script.contains("capture-pane -p -J -S -200 -t \"$TMUX_PANE\" > '/run/1.screen'"));
        // The status is written whole: to a temporary name, then renamed.
        assert!(script.contains(
            "printf %s \"$s\" > '/run/it'\\''s/1.status.tmp' && mv '/run/it'\\''s/1.status.tmp' '/run/it'\\''s/1.status'"
        ));
        assert!(script.ends_with("exit \"$s\"\n"));

        let script = build_script(&strings(&["make && make test"]), None, true);
        assert_eq!(script, "make && make test\ns=$?\nexec \"${SHELL:-sh}\"\n");
    }

    #[test]
    fn test_tmux_args_place_the_pane_without_taking_focus() {
        let mut o = parse_run_args(&strings(&["--name", "tests", "true"])).unwrap();
        assert_eq!(
            tmux_args(&o, Some("%4"), "true"),
            strings(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-n",
                "tests",
                "-a",
                "-t",
                "%4",
                "/bin/sh -c 'true'"
            ])
        );
        o.placement = Placement::Left;
        o.ratio = Some(30);
        o.focus = true;
        o.cwd = Some("/src".into());
        assert_eq!(
            tmux_args(&o, Some("%4"), "it's"),
            strings(&[
                "split-window",
                "-h",
                "-b",
                "-P",
                "-F",
                "#{pane_id}",
                "-l",
                "30%",
                "-t",
                "%4",
                "-c",
                "/src",
                "/bin/sh -c 'it'\\''s'"
            ])
        );
        o.placement = Placement::Above;
        assert_eq!(
            tmux_args(&o, None, "x")[..3],
            strings(&["split-window", "-v", "-b"])
        );
    }
}
