//! Claude Code: real utilisation from the `rate_limits` its statusLine JSON carries, a burn rate
//! from the samples `quota statusline` records, and sessions and prompts from `~/.claude`.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{epoch, num, opt_num, r1, render};

const LATEST: &str = "claude.json";
const SAMPLES: &str = "claude-samples.tsv";
/// A heartbeat sample every 5 min keeps idle periods flattening the slope.
const HEARTBEAT_SECS: i64 = 300;
const SAMPLES_MAX: usize = 6000;
const SAMPLES_KEEP: usize = 4000;

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || PathBuf::from(crate::sys::expand_home("~/.claude")),
            PathBuf::from,
        )
}

struct Sample {
    ts: i64,
    five: Option<f64>,
    reset: Option<i64>,
}

/// What the model reads; gathered from disk by `inputs`, built by hand in tests.
struct Inputs {
    available: bool,
    latest: Value,
    samples: Vec<Sample>,
    sessions: u32,
    active: u32,
    prompts: Vec<i64>,
}

pub fn agent(state: &Path, now: i64, session_rate: f64, line_only: bool) -> Value {
    let icon = crate::icons::Registry::global().agent("claude").to_string();
    model(&inputs(state, now, line_only), now, session_rate, &icon)
}

fn inputs(state: &Path, now: i64, line_only: bool) -> Inputs {
    let dir = claude_dir();
    let latest = fs::read_to_string(state.join(LATEST))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let samples = fs::read_to_string(state.join(SAMPLES))
        .map(|s| parse_samples(&s, now - 5400))
        .unwrap_or_default();
    let sessions = live_sessions(&dir.join("sessions"));
    let history = dir.join("history.jsonl");
    let active = active_sessions(&tail_lines(&history, 500), now - 1800).min(sessions);
    // The statusLine line never shows prompt counts: skip the bigger history read there.
    let prompts = if line_only {
        Vec::new()
    } else {
        prompt_times(&tail_lines(&history, 6000), now - 691_200)
    };
    Inputs {
        available: crate::sys::find_in_path("claude").is_some(),
        latest,
        samples,
        sessions,
        active,
        prompts,
    }
}

fn parse_samples(content: &str, cut: i64) -> Vec<Sample> {
    content
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let ts = f.next()?.trim().parse::<i64>().ok()?;
            let five = f.next().and_then(|v| v.trim().parse::<f64>().ok());
            let _seven = f.next();
            let reset = f.next().and_then(|r| epoch(&Value::String(r.to_string())));
            (ts >= cut).then_some(Sample { ts, five, reset })
        })
        .collect()
}

/// Sessions whose process is still a running `claude` (session files are named by pid).
fn live_sessions(dir: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = name
            .strip_suffix(".json")
            .map(|s| s.split('.').next().unwrap_or(s))
        else {
            continue;
        };
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() == "claude" {
            n += 1;
        }
    }
    n
}

/// Distinct sessions prompted since `cut`: sessions that actually burn quota (idle panes do not).
fn active_sessions(lines: &[String], cut: i64) -> u32 {
    #[allow(clippy::cast_precision_loss)] // epoch seconds are far below 2^52
    let cut_f = cut as f64;
    let ids: HashSet<String> = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["timestamp"].as_f64().is_some_and(|t| t / 1000.0 >= cut_f))
        .map(|v| v["sessionId"].to_string())
        .collect();
    u32::try_from(ids.len()).unwrap_or(u32::MAX)
}

/// Epoch seconds of real prompts (slash commands excluded) since `cut`.
pub(super) fn prompt_times(lines: &[String], cut: i64) -> Vec<i64> {
    #[allow(clippy::cast_possible_truncation)] // millisecond timestamps / 1000 fit an i64
    lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"].as_str().unwrap_or("prompt") != "slash_command")
        .filter_map(|v| v["timestamp"].as_f64().map(|t| (t / 1000.0).floor() as i64))
        .filter(|&t| t >= cut)
        .collect()
}

/// The last `n` lines of a file, read from the end so a long history costs the same as a short one.
fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return Vec::new();
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = len;
    let chunk = 64 * 1024;
    while pos > 0 && bytecount_newlines(&buf) <= n {
        let step = chunk.min(pos);
        pos -= step;
        let mut piece = vec![0; usize::try_from(step).unwrap_or(0)];
        if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut piece).is_err() {
            return Vec::new();
        }
        piece.extend_from_slice(&buf);
        buf = piece;
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| (*l).to_string()).collect()
}

fn bytecount_newlines(buf: &[u8]) -> usize {
    #[allow(clippy::naive_bytecount)] // a few 64 KiB chunks; not worth a dependency
    buf.iter().filter(|&&b| b == b'\n').count()
}

fn as_pct(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `used_percentage`, or `utilization` from older Claude Code releases.
fn used(window: &Value) -> Option<f64> {
    let u = &window["used_percentage"];
    as_pct(if u.is_null() {
        &window["utilization"]
    } else {
        u
    })
}

#[allow(clippy::cast_precision_loss)] // counts and epoch deltas are far below 2^52
fn model(inp: &Inputs, now: i64, srate: f64, icon: &str) -> Value {
    let l = &inp.latest;
    let r5 = epoch(&l["five_hour"]["resets_at"]);
    let r7 = epoch(&l["seven_day"]["resets_at"]);
    // A window whose reset has passed starts again from zero.
    let five = if r5.is_some_and(|r| r <= now) {
        Some(0.0)
    } else {
        used(&l["five_hour"])
    };
    let seven = if r7.is_some_and(|r| r <= now) {
        Some(0.0)
    } else {
        used(&l["seven_day"])
    };

    let win: Vec<&Sample> = inp
        .samples
        .iter()
        .filter(|s| s.ts >= now - 3600 && s.five.is_some() && s.reset == r5)
        .collect();
    let rate = match (win.first(), win.last()) {
        (Some(a), Some(b)) if win.len() >= 2 && b.ts - a.ts >= 300 => {
            let r =
                (b.five.unwrap_or(0.0) - a.five.unwrap_or(0.0)) / ((b.ts - a.ts) as f64 / 3600.0);
            Some(r.max(0.0))
        }
        _ => None,
    };
    let act = f64::from(inp.active);
    let per = match rate {
        Some(r) if r > 0.0 && inp.active > 0 => r / act,
        _ => srate,
    };
    let rate_new = rate.unwrap_or(act * per) + per;
    let h5 = r5
        .filter(|&r| r > now)
        .map_or(5.0, |r| (r - now) as f64 / 3600.0)
        .min(5.0);
    let proj = five.map(|f| f + rate_new * h5);
    let eta_h = five
        .filter(|_| rate_new > 0.0)
        .map(|f| (100.0 - f) / rate_new);
    let pace = match (r7, seven) {
        (Some(r), Some(s)) => {
            let elapsed = (1.0 - (r - now) as f64 / 604_800.0).clamp(0.02, 1.0);
            Some(s / (elapsed * 100.0))
        }
        _ => None,
    };

    let five0 = five.unwrap_or(0.0);
    let seven0 = seven.unwrap_or(0.0);
    let level = match five {
        None => "unknown",
        Some(f) if f >= 98.0 || seven0 >= 98.0 => "red",
        Some(_) if rate.is_some() && eta_h.is_some_and(|e| e < 0.5) && h5 > 0.5 => "red",
        Some(f)
            if f >= 80.0
                || (rate.is_some() && proj.unwrap_or(0.0) >= 90.0)
                || seven0 >= 85.0
                || (pace.unwrap_or(0.0) >= 1.25 && seven0 >= 40.0) =>
        {
            "yellow"
        }
        Some(_) => "green",
    };
    let until = if five0 >= 98.0 {
        r5
    } else if seven0 >= 98.0 {
        r7
    } else {
        None
    };
    let count_since = |since: i64| inp.prompts.iter().filter(|&&t| t >= since).count();

    json!({
        "agent": "claude",
        "title": "Claude Code",
        "available": inp.available,
        "icon": icon,
        "sessions": inp.sessions,
        "active": inp.active,
        "five": opt_num(r1(five)),
        "seven": opt_num(r1(seven)),
        "five_reset": r5,
        "seven_reset": r7,
        "rate": opt_num(r1(rate)),
        "rate_with_new": num(r1(Some(rate_new)).unwrap_or(0.0)),
        "projected_five": opt_num(r1(proj)),
        "eta_full_h": opt_num(r1(eta_h)),
        "week_pace": opt_num(r1(pace)),
        "prompts_1h": count_since(now - 3600),
        "prompts_5h": count_since(now - 18000),
        "activity": render::activity_bins(&inp.prompts, now),
        "stale_s": l["ts"].as_i64().map(|ts| now - ts),
        "level": level,
        "until": until,
    })
}

/// `quota statusline`: records Claude Code's `rate_limits` and prints the model, context and
/// quota as one line.
pub fn run_statusline() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let state = super::state_dir();
    let _ = fs::create_dir_all(&state);
    let now = chrono::Utc::now().timestamp();
    record(&state, &payload, now);

    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = payload["model"]["display_name"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        parts.push(name.to_string());
    }
    if let Some(ctx) = payload["context_window"]["used_percentage"].as_f64() {
        parts.push(format!("ctx {}%", ctx.floor()));
    }
    let head = parts.join(" · ");
    let snap = super::Snapshot::collect(true, false);
    let quota = render::line(&snap);
    let sep = if !head.is_empty() && !quota.is_empty() {
        " · "
    } else {
        ""
    };
    println!("{head}{sep}{quota}");
    0
}

fn record(state: &Path, payload: &Value, now: i64) {
    let rl = &payload["rate_limits"];
    if !rl.is_object() {
        return;
    }
    let mut latest = serde_json::Map::new();
    latest.insert("ts".into(), json!(now));
    if let Some(obj) = rl.as_object() {
        latest.extend(obj.clone());
    }
    let tmp = state.join(format!("{LATEST}.tmp"));
    if fs::write(&tmp, Value::Object(latest).to_string()).is_ok() {
        let _ = fs::rename(&tmp, state.join(LATEST));
    }

    let field = |v: &Value| match v {
        Value::Null => "-".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let pick = |w: &Value| {
        let u = &w["used_percentage"];
        field(if u.is_null() { &w["utilization"] } else { u })
    };
    let five = pick(&rl["five_hour"]);
    let seven = pick(&rl["seven_day"]);
    let reset = field(&rl["five_hour"]["resets_at"]);

    let path = state.join(SAMPLES);
    let content = fs::read_to_string(&path).unwrap_or_default();
    let last = content.lines().last().unwrap_or("");
    let mut f = last.split('\t');
    let l_ts = f.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let l_five = f.next().unwrap_or("-");
    let l_seven = f.next().unwrap_or("-");
    if five == l_five && seven == l_seven && now - l_ts < HEARTBEAT_SECS {
        return;
    }
    let row = format!("{now}\t{five}\t{seven}\t{reset}\n");
    let lines = content.lines().count() + 1;
    if lines > SAMPLES_MAX {
        let mut kept: Vec<&str> = content.lines().collect();
        kept.drain(..kept.len().saturating_sub(SAMPLES_KEEP - 1));
        let body = format!("{}\n{row}", kept.join("\n"));
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, body).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    } else if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(row.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn inputs(latest: Value, samples: Vec<Sample>, active: u32) -> Inputs {
        Inputs {
            available: true,
            latest,
            samples,
            sessions: active,
            active,
            prompts: Vec::new(),
        }
    }

    fn latest(five: f64, seven: f64, r5: i64, r7: i64) -> Value {
        json!({"ts": NOW, "five_hour": {"used_percentage": five, "resets_at": r5},
               "seven_day": {"used_percentage": seven, "resets_at": r7}})
    }

    #[test]
    fn test_burn_rate_projection_and_level_match_the_contract() {
        // 30% -> 42% over 40 min is 18 %/h; with no active session a new one costs the
        // session rate (10): 42 + (18 + 10) x 2h = 98 at the reset -> yellow.
        let r5 = NOW + 7200;
        let samples = vec![
            Sample {
                ts: NOW - 2400,
                five: Some(30.0),
                reset: Some(r5),
            },
            Sample {
                ts: NOW - 1200,
                five: Some(36.0),
                reset: Some(r5),
            },
            Sample {
                ts: NOW,
                five: Some(42.0),
                reset: Some(r5),
            },
        ];
        let a = model(
            &inputs(latest(42.0, 22.0, r5, NOW + 3 * 86400), samples, 0),
            NOW,
            10.0,
            "C",
        );
        assert_eq!(a["rate"], json!(18));
        assert_eq!(a["projected_five"], json!(98));
        assert_eq!(a["level"], "yellow");
        assert_eq!(a["seven_reset"], json!(NOW + 3 * 86400));
    }

    #[test]
    fn test_level_red_at_cap_and_until_is_the_reset() {
        let r5 = NOW + 600;
        let a = model(
            &inputs(latest(99.0, 10.0, r5, NOW + 86400), vec![], 0),
            NOW,
            10.0,
            "C",
        );
        assert_eq!(a["level"], "red");
        assert_eq!(a["until"], json!(r5));
        let none = model(&inputs(Value::Null, vec![], 0), NOW, 10.0, "C");
        assert_eq!(none["level"], "unknown");
        assert_eq!(none["stale_s"], Value::Null);
    }

    #[test]
    fn test_passed_reset_starts_the_window_from_zero() {
        let a = model(
            &inputs(latest(99.0, 50.0, NOW - 1, NOW + 86400), vec![], 0),
            NOW,
            10.0,
            "C",
        );
        assert_eq!(a["five"], json!(0));
        assert_eq!(a["level"], "green");
    }

    #[test]
    fn test_record_appends_on_change_or_heartbeat_only() {
        let dir = std::env::temp_dir().join(format!("waymux-quota-rec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let payload = |five: i64| {
            json!({"rate_limits": {"five_hour": {"used_percentage": five, "resets_at": NOW + 60},
                                   "seven_day": {"used_percentage": 22, "resets_at": "2027-01-01T00:00:00.000Z"}}})
        };
        record(&dir, &payload(42), NOW);
        record(&dir, &payload(42), NOW + 10);
        record(&dir, &payload(43), NOW + 20);
        record(&dir, &payload(43), NOW + 20 + HEARTBEAT_SECS);
        let rows = fs::read_to_string(dir.join(SAMPLES)).unwrap();
        assert_eq!(rows.lines().count(), 3, "{rows}");
        assert!(
            rows.starts_with(&format!("{NOW}\t42\t22\t{}", NOW + 60)),
            "{rows}"
        );
        let latest: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(LATEST)).unwrap()).unwrap();
        assert_eq!(latest["ts"], json!(NOW + 20 + HEARTBEAT_SECS));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_history_counts_prompts_and_active_sessions() {
        let lines: Vec<String> = [
            r#"{"timestamp": 1800000000000, "sessionId": "a"}"#,
            r#"{"timestamp": 1799999000000, "sessionId": "b", "type": "slash_command"}"#,
            r#"{"timestamp": 1700000000000, "sessionId": "c"}"#,
            "not json",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(prompt_times(&lines, NOW - 3600), vec![NOW]);
        assert_eq!(active_sessions(&lines, NOW - 1800), 2);
    }

    #[test]
    fn test_tail_lines_reads_only_the_end() {
        let path = std::env::temp_dir().join(format!("waymux-quota-tail-{}", std::process::id()));
        let body: String = (0..5000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, body).unwrap();
        let tail = tail_lines(&path, 3);
        assert_eq!(tail, vec!["line 4997", "line 4998", "line 4999"]);
        assert!(tail_lines(Path::new("/nonexistent/x"), 3).is_empty());
        let _ = fs::remove_file(&path);
    }
}
