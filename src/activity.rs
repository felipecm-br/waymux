//! What each agent did since you last focused its pane: finished turns, files it edited,
//! commands it ran and the last test command. The daemon counts it from the hook's tool
//! summaries (`Bash: cargo test`, `Edit: retry.rs`); focusing the pane (`agentState/dismiss`
//! on `pane-focus-in`) starts the count again. The Cockpit preview shows it as one line once
//! you have been away from the pane for [`AWAY_MS`].

use crate::adapters::AgentState;
use serde::{Deserialize, Serialize};

/// How long you must have been away from a pane before its recap shows.
pub const AWAY_MS: u64 = 10 * 60 * 1000;
/// A turn counts once it has worked this long: a redraw that flips a pane to working for an
/// instant is not a turn.
pub const MIN_TURN_MS: u64 = 5000;
const MAX_FILES: usize = 200;
const MAX_COMMAND_CHARS: usize = 80;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub turns: u64,
    pub commands: u64,
    /// Files edited, each once, in the order first edited.
    pub files: Vec<String>,
    pub last_command: Option<String>,
    pub last_test: Option<String>,
    /// Epoch ms of the last focus; 0 when not focused since the daemon started.
    pub seen_at: u64,
    /// Epoch ms the count started (the last focus, or when the daemon first saw the pane).
    pub since: u64,
    #[serde(skip)]
    working_since: Option<u64>,
}

fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool,
        "Bash" | "bash" | "shell" | "run_shell_command" | "exec_command" | "local_shell"
    )
}

fn is_edit_tool(tool: &str) -> bool {
    matches!(
        tool,
        "Edit"
            | "Write"
            | "MultiEdit"
            | "NotebookEdit"
            | "edit"
            | "write"
            | "write_file"
            | "replace"
            | "apply_patch"
            | "str_replace_based_edit_tool"
    )
}

/// Whether a shell command runs a test suite.
pub fn is_test_command(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    [
        "cargo test",
        "cargo nextest",
        "pytest",
        "go test",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "bun test",
        "jest",
        "vitest",
        "rspec",
        "make test",
        "mix test",
        "ctest",
        ".test.sh",
    ]
    .iter()
    .any(|t| c.contains(t))
}

fn short(cmd: &str) -> String {
    let flat = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_COMMAND_CHARS {
        format!(
            "{}…",
            flat.chars().take(MAX_COMMAND_CHARS).collect::<String>()
        )
    } else {
        flat
    }
}

impl Activity {
    pub fn new(now_ms: u64) -> Self {
        Self {
            since: now_ms,
            ..Self::default()
        }
    }

    /// Counts one state change. `message` is the hook's tool summary on `working`; `dismissed`
    /// marks the idle a focus sets to clear a prompt badge, which ends no turn.
    pub fn record(&mut self, state: &AgentState, message: Option<&str>, dismissed: bool, now: u64) {
        match state {
            AgentState::Working => {
                self.working_since.get_or_insert(now);
                let Some((tool, detail)) = message.and_then(|m| m.split_once(": ")) else {
                    return;
                };
                if is_shell_tool(tool) {
                    self.commands += 1;
                    let cmd = short(detail);
                    if is_test_command(detail) {
                        self.last_test = Some(cmd.clone());
                    }
                    self.last_command = Some(cmd);
                } else if is_edit_tool(tool)
                    && !detail.is_empty()
                    && self.files.len() < MAX_FILES
                    && !self.files.iter().any(|f| f == detail)
                {
                    self.files.push(detail.to_string());
                }
            }
            AgentState::Idle if !dismissed => {
                if let Some(start) = self.working_since.take()
                    && now.saturating_sub(start) >= MIN_TURN_MS
                {
                    self.turns += 1;
                }
            }
            _ => {}
        }
    }

    /// The pane was focused: what happened so far has been seen.
    pub fn mark_seen(&mut self, now: u64) {
        *self = Self {
            seen_at: now,
            since: now,
            working_since: self.working_since,
            ..Self::default()
        };
    }

    fn is_empty(&self) -> bool {
        self.turns == 0 && self.commands == 0 && self.files.is_empty()
    }
}

/// One line for the Cockpit preview, or `None` when there is nothing new or you looked at the
/// pane less than [`AWAY_MS`] ago.
pub fn recap_line(a: &Activity, now: u64) -> Option<String> {
    if a.is_empty() || (a.seen_at > 0 && now.saturating_sub(a.seen_at) < AWAY_MS) {
        return None;
    }
    let age = crate::cockpit::items::format_age(now.saturating_sub(a.since) / 1000);
    let head = if a.seen_at > 0 {
        format!("Since you last looked ({age})")
    } else {
        format!("Since the daemon started ({age})")
    };
    let plural = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let mut parts = Vec::new();
    if a.turns > 0 {
        parts.push(plural(a.turns, "turn"));
    }
    if !a.files.is_empty() {
        let n = a.files.len();
        let names = a
            .files
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = if n > 2 {
            format!(" +{}", n - 2)
        } else {
            String::new()
        };
        parts.push(format!(
            "{}: {names}{more}",
            plural(u64::try_from(n).unwrap_or(u64::MAX), "file")
        ));
    }
    if a.commands > 0 {
        parts.push(plural(a.commands, "command"));
    }
    if let Some(t) = &a.last_test {
        parts.push(format!("tests: {t}"));
    }
    Some(format!("󰋚 {head}: {}", parts.join(" · ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(a: &mut Activity, msg: &str, now: u64) {
        a.record(&AgentState::Working, Some(msg), false, now);
    }

    #[test]
    fn test_counts_turns_commands_files_and_tests() {
        let mut a = Activity::new(0);
        work(&mut a, "Bash: cargo build", 1_000);
        work(&mut a, "Edit: retry.rs", 2_000);
        work(&mut a, "Edit: retry.rs", 2_500);
        work(&mut a, "Write: retry_test.rs", 3_000);
        work(&mut a, "Bash: cargo test --lib", 4_000);
        a.record(&AgentState::Permission, Some("Bash: rm x"), false, 5_000);
        a.record(&AgentState::Idle, None, false, 9_000);
        assert_eq!(a.turns, 1);
        assert_eq!(a.commands, 2);
        assert_eq!(a.files, ["retry.rs", "retry_test.rs"]);
        assert_eq!(a.last_test.as_deref(), Some("cargo test --lib"));
        assert_eq!(a.last_command.as_deref(), Some("cargo test --lib"));
    }

    #[test]
    fn test_short_and_dismissed_idles_are_not_turns() {
        let mut a = Activity::new(0);
        a.record(&AgentState::Working, None, false, 1_000);
        a.record(&AgentState::Idle, None, false, 2_000);
        assert_eq!(a.turns, 0, "a 1 s flicker is not a turn");

        a.record(&AgentState::Working, None, false, 10_000);
        a.record(&AgentState::Idle, None, true, 20_000);
        assert_eq!(a.turns, 0, "a focus dismissing a badge ends no turn");
        a.record(&AgentState::Idle, None, false, 21_000);
        assert_eq!(a.turns, 1);
    }

    #[test]
    fn test_mark_seen_resets_but_keeps_a_running_turn() {
        let mut a = Activity::new(0);
        work(&mut a, "Bash: ls", 1_000);
        a.mark_seen(2_000);
        assert_eq!(a.commands, 0);
        assert_eq!(a.seen_at, 2_000);
        a.record(&AgentState::Idle, None, false, 8_000);
        assert_eq!(
            a.turns, 1,
            "the turn that was running when you looked still ends"
        );
    }

    #[test]
    fn test_recap_line_waits_for_away_and_names_files() {
        let mut a = Activity::new(0);
        a.mark_seen(1_000);
        for (i, f) in ["a.rs", "b.rs", "c.rs", "d.rs"].iter().enumerate() {
            work(&mut a, &format!("Edit: {f}"), 2_000 + i as u64);
        }
        work(&mut a, "Bash: go test ./...", 3_000);
        a.record(&AgentState::Idle, None, false, 9_000);

        assert_eq!(
            recap_line(&a, 1_000 + AWAY_MS - 1),
            None,
            "not away long enough"
        );
        let line = recap_line(&a, 1_000 + 42 * 60 * 1000).unwrap();
        assert_eq!(
            line,
            "󰋚 Since you last looked (42m): 1 turn · 4 files: a.rs, b.rs +2 · 1 command · tests: go test ./..."
        );
    }

    #[test]
    fn test_recap_line_none_when_nothing_happened() {
        let a = Activity::new(0);
        assert_eq!(recap_line(&a, AWAY_MS * 3), None);
    }

    #[test]
    fn test_is_test_command() {
        assert!(is_test_command("cd x && cargo test -q"));
        assert!(is_test_command("bash tests/key_telemetry.test.sh"));
        assert!(!is_test_command("cargo build"));
    }
}
