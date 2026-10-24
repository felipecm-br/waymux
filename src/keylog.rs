//! Key-press telemetry: one TSV line per key the operator presses in the Cockpit or in a
//! wrapped tmux binding, so a report can show what is used, what is not, and where a shorter
//! path existed. Lines go to `$XDG_STATE_HOME/flow/keys.tsv` (default `~/.local/state`):
//!
//! `epoch_secs  context  key  state  target  origin`
//!
//! `state` is the agent state of the Cockpit row the key acted on, `target` that row's
//! `session:window`, `origin` the window the Cockpit was opened from (`WAYMUX_KEYLOG_ORIGIN`).
//! `WAYMUX_KEYLOG=0` turns it off. Logging never fails the caller.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Environment variable the Cockpit sets on its picker so `keylog` calls know the origin window.
pub const ORIGIN_ENV: &str = "WAYMUX_KEYLOG_ORIGIN";

fn log_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("flow").join("keys.tsv"))
}

/// Tabs and newlines would split the record; everything else is kept.
fn field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn line(ts: u64, fields: &[&str]) -> String {
    let mut out = ts.to_string();
    for f in fields {
        out.push('\t');
        out.push_str(&field(f));
    }
    out.push('\n');
    out
}

/// Appends one key press. `origin` falls back to `WAYMUX_KEYLOG_ORIGIN`.
pub fn record(context: &str, key: &str, state: &str, target: &str, origin: &str) {
    if std::env::var("WAYMUX_KEYLOG").is_ok_and(|v| v == "0") || key.is_empty() {
        return;
    }
    let Some(path) = log_path() else {
        return;
    };
    let origin = if origin.is_empty() {
        std::env::var(ORIGIN_ENV).unwrap_or_default()
    } else {
        origin.to_string()
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line(ts, &[context, key, state, target, &origin]).as_bytes());
    }
}

/// `waymux keylog <context> <key> [state] [target] [origin]`; always exits 0.
pub fn run_cli(args: &[String]) -> i32 {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    record(arg(0), arg(1), arg(2), arg(3), arg(4));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_keeps_columns_aligned() {
        assert_eq!(
            line(7, &["cockpit", "g", "review", "s:1", "s:1"]),
            "7\tcockpit\tg\treview\ts:1\ts:1\n"
        );
        assert_eq!(
            line(7, &["tmux", "a\tb\nc", "", "", ""]),
            "7\ttmux\ta b c\t\t\t\n"
        );
    }
}
