//! Key-press telemetry: one TSV line per key the operator presses in the Cockpit or in a
//! wrapped tmux binding, so a report can show what is used, what is not, and where a shorter
//! path existed. Lines go to `$XDG_STATE_HOME/flow/keys.tsv` (default `~/.local/state`):
//!
//! `epoch_secs  context  key  state  target  origin`
//!
//! `state` is the agent state of the Cockpit row the key acted on, `target` that row's
//! `session:window`, `origin` the window the Cockpit was opened from (`WAYMUX_KEYLOG_ORIGIN`).
//! Off unless `keylog = true` in `config.toml` or `WAYMUX_KEYLOG=1`; `WAYMUX_KEYLOG=0` always
//! turns it off. Logging never fails the caller.

use crate::config::Config;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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

/// `WAYMUX_KEYLOG=0` always disables; `WAYMUX_KEYLOG=1` enables; anything else defers to the
/// `keylog` setting in `config.toml` (false when unset).
fn is_enabled(env_val: Option<&str>, config_keylog: bool) -> bool {
    match env_val {
        Some("0") => false,
        Some("1") => true,
        _ => config_keylog,
    }
}

fn enabled() -> bool {
    let env_val = std::env::var("WAYMUX_KEYLOG").ok();
    if matches!(env_val.as_deref(), Some("0" | "1")) {
        return is_enabled(env_val.as_deref(), false);
    }
    let config_keylog = Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .is_some_and(|c| c.keylog);
    is_enabled(env_val.as_deref(), config_keylog)
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

fn append_to(path: &Path, ts: u64, fields: &[&str]) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line(ts, fields).as_bytes());
    }
}

/// Appends one key press. `origin` falls back to `WAYMUX_KEYLOG_ORIGIN`.
pub fn record(context: &str, key: &str, state: &str, target: &str, origin: &str) {
    if key.is_empty() || !enabled() {
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
        .map_or(0, |d| d.as_secs());
    append_to(&path, ts, &[context, key, state, target, &origin]);
}

/// `waymux keylog <context> <key> [state] [target] [origin]`; always exits 0.
pub fn run_cli(args: &[String]) -> i32 {
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
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

    #[test]
    fn test_is_enabled_is_off_by_default_and_zero_always_wins() {
        assert!(!is_enabled(None, false));
        assert!(!is_enabled(Some(""), false));
        assert!(is_enabled(None, true));
        assert!(is_enabled(Some("1"), false));
        assert!(!is_enabled(Some("0"), false));
        assert!(!is_enabled(Some("0"), true));
    }

    #[test]
    fn test_append_to_creates_parent_and_appends_rows() {
        let dir = std::env::temp_dir().join(format!("waymux-keylog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("flow").join("keys.tsv");
        append_to(&path, 10, &["cockpit", "g", "review", "s:1", "s:0"]);
        append_to(&path, 11, &["cockpit", "esc", "", "", "s:0"]);
        let written = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            written,
            "10\tcockpit\tg\treview\ts:1\ts:0\n11\tcockpit\tesc\t\t\ts:0\n"
        );
    }
}
