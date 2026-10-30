//! `waymux quota`: agent quota telemetry and the agent a new session should launch.
//!
//! Claude Code is built in: its statusLine JSON carries `rate_limits`, which `quota statusline`
//! records as samples (burn rate in %/h), and its live sessions and prompts come from
//! `~/.claude`. Other agents come from providers (`[quota] providers`, or one command per line
//! in `$WAYMUX_QUOTA_PROVIDERS`): commands that print a JSON array of agent objects with at
//! least `agent`, `available` and `level` (`green`, `yellow`, `red`, `unknown`). Optional
//! fields shape the report: `title`, `icon`, `tag`, `sessions`, `prompts_1h`, `prompts_5h`,
//! `activity` (10 prompt counts, one per 30 min, oldest first), `until` (epoch the agent is
//! out of quota until), `groups` (`[{key, label, pct, reset, abbr?, detail?, hidden?}]`, `pct`
//! is percent left), `est_five` / `capacity_5h` / `window` (an estimate without live data),
//! `default_model`, and `notes` (`[{text, level?}]`, `level` `warn` for yellow). Every field
//! is passed through to `--json` unchanged.

mod claude;
mod popup;
mod render;

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{Config, QuotaConfig};

const USAGE: &str = "\
usage: waymux quota [MODE]

  (none) | --report   Human-readable report
  -v | --visual       Gauges, pace markers and 5h sparklines
  --popup [dir]       The visual report in a tmux popup, with launch keys
  --json              Machine-readable snapshot (agents, levels, recommendation)
  --badge             tmux-format badge: every agent, its quota, the recommendation
  --badge-min         One level-colored glyph: the agent a new session launches
  --line              Claude's one-line summary (what `statusline` appends)
  --recommend         Print the agent to launch
  --refresh           Re-read every provider and rewrite the badge caches (silent)
  statusline          Claude Code statusLine command: records rate_limits from stdin

Environment: WAYMUX_QUOTA_PROVIDERS, WAYMUX_QUOTA_PREFERENCE (space-separated agents),
WAYMUX_QUOTA_SESSION_RATE, WAYMUX_QUOTA_STATE_DIR, CLAUDE_CONFIG_DIR.";

pub fn run_cli(args: &[String]) -> i32 {
    let mode = args.first().map_or("", String::as_str);
    match mode {
        "statusline" => claude::run_statusline(),
        "--refresh" => {
            refresh();
            0
        }
        "--popup" => popup::run(args.get(1).map(String::as_str)),
        "-h" | "--help" => {
            println!("{USAGE}");
            0
        }
        "" | "--report" | "--json" | "--badge" | "--badge-min" | "--line" | "--recommend"
        | "-v" | "--visual" | "--chart" | "--tui" => {
            let line_only = mode == "--line";
            let snap = Snapshot::collect(line_only, false);
            let out = match mode {
                "--json" => render::json(&snap),
                "--badge" => render::badge(&snap),
                "--badge-min" => render::badge_min(&snap),
                "--line" => render::line(&snap),
                "--recommend" => snap.recommend.clone(),
                "-v" | "--visual" | "--chart" | "--tui" => {
                    render::visual(&snap, &render::Colors::load())
                }
                _ => render::report(&snap),
            };
            if !out.is_empty() {
                println!("{out}");
            }
            0
        }
        other => {
            eprintln!("waymux quota: unknown option '{other}' (see --help)");
            2
        }
    }
}

/// Every agent's quota at one moment, and the agent a new session should launch.
pub struct Snapshot {
    pub now: i64,
    pub agents: Vec<Value>,
    pub recommend: String,
    pub reason: String,
}

impl Snapshot {
    /// `line_only` reads Claude alone (the statusLine path never waits on providers);
    /// `force` asks providers to skip their caches.
    pub(crate) fn collect(line_only: bool, force: bool) -> Self {
        let now = chrono::Utc::now().timestamp();
        let cfg = quota_config();
        let state = state_dir();
        let _ = fs::create_dir_all(&state);
        let rate = session_rate(&cfg);
        let mut agents = vec![claude::agent(&state, now, rate, line_only)];
        if !line_only {
            for cmd in providers(&cfg) {
                agents.extend(run_provider(&cmd, now, &state, force));
            }
        }
        let order = preference(&cfg, &agents);
        let (recommend, reason) = recommend(&agents, &order);
        Self {
            now,
            agents,
            recommend,
            reason,
        }
    }

    pub fn agent(&self, name: &str) -> Option<&Value> {
        self.agents.iter().find(|a| a["agent"] == name)
    }
}

fn quota_config() -> QuotaConfig {
    Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .and_then(|c| c.quota)
        .unwrap_or_default()
}

/// `$WAYMUX_QUOTA_STATE_DIR`, else `$XDG_STATE_HOME/waymux/quota`.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("WAYMUX_QUOTA_STATE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || PathBuf::from(crate::sys::expand_home("~/.local/state")),
            PathBuf::from,
        );
    base.join("waymux").join("quota")
}

fn session_rate(cfg: &QuotaConfig) -> f64 {
    std::env::var("WAYMUX_QUOTA_SESSION_RATE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .or(cfg.session_rate)
        .unwrap_or(10.0)
}

fn providers(cfg: &QuotaConfig) -> Vec<String> {
    match std::env::var("WAYMUX_QUOTA_PROVIDERS") {
        Ok(list) => list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => cfg.providers.clone(),
    }
}

fn preference(cfg: &QuotaConfig, agents: &[Value]) -> Vec<String> {
    if let Ok(list) = std::env::var("WAYMUX_QUOTA_PREFERENCE")
        && !list.trim().is_empty()
    {
        return list.split_whitespace().map(str::to_string).collect();
    }
    cfg.preference.clone().unwrap_or_else(|| {
        agents
            .iter()
            .filter_map(|a| a["agent"].as_str().map(str::to_string))
            .collect()
    })
}

/// A provider's agents: a JSON array on stdout (or `{"agents": [...]}`); anything else is
/// skipped so one broken provider never hides the rest.
fn run_provider(cmd: &str, now: i64, state: &Path, force: bool) -> Vec<Value> {
    let mut command = Command::new("sh");
    command
        .args(["-c", cmd])
        .env("WAYMUX_QUOTA_NOW", now.to_string())
        .env("WAYMUX_QUOTA_STATE_DIR", state)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if force {
        command.env("WAYMUX_QUOTA_FORCE", "1");
    }
    let Ok(out) = command.output() else {
        return Vec::new();
    };
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(Value::Array(agents)) => agents.into_iter().filter(Value::is_object).collect(),
        Ok(Value::Object(mut obj)) => match obj.remove("agents") {
            Some(Value::Array(agents)) => agents.into_iter().filter(Value::is_object).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The first available agent in `order` with headroom (green or unknown), else the first
/// yellow one, else the one whose quota comes back soonest.
fn recommend(agents: &[Value], order: &[String]) -> (String, String) {
    let cands: Vec<&Value> = order
        .iter()
        .flat_map(|o| {
            agents
                .iter()
                .filter(move |a| a["agent"] == o.as_str() && a["available"] == true)
        })
        .collect();
    let level = |a: &Value| a["level"].as_str().unwrap_or("unknown").to_string();
    let pick = cands
        .iter()
        .find(|a| matches!(level(a).as_str(), "green" | "unknown"))
        .or_else(|| cands.iter().find(|a| level(a) == "yellow"))
        .or_else(|| {
            cands
                .iter()
                .min_by_key(|a| a["until"].as_i64().unwrap_or(0))
        });
    let rec = pick.map_or_else(
        || order.first().cloned().unwrap_or_else(|| "claude".into()),
        |a| a["agent"].as_str().unwrap_or_default().to_string(),
    );
    let why = match cands.first() {
        None => "no agent available".to_string(),
        Some(first) if first["agent"] == rec.as_str() => "preferred agent has headroom".into(),
        Some(first) => format!(
            "{} is {}; falling back",
            first["agent"].as_str().unwrap_or_default(),
            level(first)
        ),
    };
    (rec, why)
}

/// `--refresh`: providers skip their caches, then the badge caches are rewritten. Runs one
/// at a time (a second caller exits at once), so timers and readers never stack up.
fn refresh() {
    use nix::fcntl::{Flock, FlockArg};
    let state = state_dir();
    let _ = fs::create_dir_all(&state);
    let Ok(file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("refresh.lock"))
    else {
        return;
    };
    let Ok(_lock) = Flock::lock(file, FlockArg::LockExclusiveNonblock) else {
        return;
    };
    let snap = Snapshot::collect(false, true);
    write_cache(&state.join("badge"), &render::badge(&snap));
    write_cache(&state.join("badge-min"), &render::badge_min(&snap));
}

/// Atomic replace; an empty render keeps the previous cache.
fn write_cache(path: &Path, content: &str) {
    if content.is_empty() {
        return;
    }
    let tmp = path.with_extension("tmp");
    let ok = fs::File::create(&tmp)
        .and_then(|mut f| writeln!(f, "{content}"))
        .is_ok();
    if ok {
        let _ = fs::rename(&tmp, path);
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

/// jq-style number: integral values print without a fraction.
pub(crate) fn num(x: f64) -> Value {
    #[allow(clippy::cast_possible_truncation)] // checked integral and in range first
    if x.fract() == 0.0 && x.abs() < 9e15 {
        Value::from(x as i64)
    } else {
        Value::from(x)
    }
}

pub(crate) fn opt_num(x: Option<f64>) -> Value {
    x.map_or(Value::Null, num)
}

/// Rounds to one decimal, as the report shows rates and projections.
pub(crate) fn r1(x: Option<f64>) -> Option<f64> {
    x.map(|v| (v * 10.0).round() / 10.0)
}

/// Seconds since the epoch from a JSON timestamp: a number (milliseconds when > 1e12), an
/// ISO 8601 UTC string, or a numeric string.
pub(crate) fn epoch(v: &Value) -> Option<i64> {
    #[allow(clippy::cast_possible_truncation)] // epoch seconds fit an i64
    match v {
        Value::Number(n) => {
            let x = n.as_f64()?;
            Some(if x > 1e12 { x / 1000.0 } else { x } as i64)
        }
        Value::String(s) => parse_iso(s).or_else(|| s.trim().parse::<f64>().ok().map(|x| x as i64)),
        _ => None,
    }
}

fn parse_iso(s: &str) -> Option<i64> {
    // Drop the first fractional part and a `+00:00` suffix, like jq's `fromdateiso8601` input.
    let mut t = s.to_string();
    if let Some(dot) = t.find('.') {
        let end = t[dot + 1..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(t.len(), |i| dot + 1 + i);
        if end > dot + 1 {
            t.replace_range(dot..end, "");
        }
    }
    if let Some(stripped) = t.strip_suffix("+00:00") {
        t = format!("{stripped}Z");
    }
    chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .map(|d| d.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(name: &str, level: &str, until: Option<i64>) -> Value {
        json!({"agent": name, "available": true, "level": level, "until": until})
    }

    #[test]
    fn test_recommend_prefers_headroom_then_yellow_then_soonest_reset() {
        let order: Vec<String> = ["claude", "agy", "agyy"].map(String::from).to_vec();
        let all = [
            agent("claude", "red", Some(900)),
            agent("agy", "yellow", None),
            agent("agyy", "green", None),
        ];
        assert_eq!(
            recommend(&all, &order),
            ("agyy".into(), "claude is red; falling back".into())
        );
        let no_green = [
            agent("claude", "red", Some(900)),
            agent("agy", "yellow", None),
        ];
        assert_eq!(recommend(&no_green, &order).0, "agy");
        let all_red = [
            agent("claude", "red", Some(900)),
            agent("agy", "red", Some(300)),
        ];
        assert_eq!(recommend(&all_red, &order).0, "agy");
        let ok = [agent("claude", "unknown", None)];
        assert_eq!(
            recommend(&ok, &order),
            ("claude".into(), "preferred agent has headroom".into())
        );
    }

    #[test]
    fn test_recommend_skips_unavailable_and_unlisted_agents() {
        let order: Vec<String> = vec!["claude".into(), "agy".into()];
        let mut gone = agent("claude", "green", None);
        gone["available"] = json!(false);
        let agents = [
            gone,
            agent("agy", "green", None),
            agent("codex", "green", None),
        ];
        assert_eq!(recommend(&agents, &order).0, "agy");
        assert_eq!(
            recommend(&[], &order),
            ("claude".into(), "no agent available".into())
        );
    }

    #[test]
    fn test_epoch_reads_numbers_millis_iso_and_numeric_strings() {
        assert_eq!(epoch(&json!(1_700_000_000)), Some(1_700_000_000));
        assert_eq!(epoch(&json!(1_700_000_000_123_i64)), Some(1_700_000_000));
        assert_eq!(
            epoch(&json!("2023-11-14T22:13:20.000Z")),
            Some(1_700_000_000)
        );
        assert_eq!(
            epoch(&json!("2023-11-14T22:13:20+00:00")),
            Some(1_700_000_000)
        );
        assert_eq!(epoch(&json!("1700000000")), Some(1_700_000_000));
        assert_eq!(epoch(&json!("-")), None);
        assert_eq!(epoch(&Value::Null), None);
    }

    #[test]
    fn test_num_prints_integral_values_without_fraction() {
        assert_eq!(num(18.0).to_string(), "18");
        assert_eq!(num(1.3).to_string(), "1.3");
        assert_eq!(r1(Some(1.25)), Some(1.3));
        assert_eq!(r1(None), None);
    }
}
