/// The unit `install.sh` and `init --systemd` write; one file so the two cannot drift.
const SYSTEMD_UNIT: &str = include_str!("../systemd/waymux.service");

const TMUX_SNIPPET: &str = "\
# Waymux tmux integration. Easiest: load the plugin installed by install.sh,
# which sets everything below and honors @waymux_key_* overrides.
run-shell \"~/.local/share/waymux/tmux/waymux.tmux\"

# Or bind by hand:
# set -g status-interval 0
# set-hook -g pane-focus-in 'run-shell -b \"waymux dismiss #{pane_id} >/dev/null 2>&1 || true\"'
# bind-key Space if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell \"waymux cockpit\"'
# bind-key i     if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell \"waymux triage --popup\"'
# bind-key I     run-shell \"waymux triage --jump\"
# bind-key -n C-S-i run-shell \"waymux triage --jump\"";

/// `(event, matcher, commands)` for Claude Code's `hooks` block. The `claude` hint names the
/// agent where the hook JSON alone cannot (window icon, wait reasons).
const CLAUDE_HOOKS: &[(&str, Option<&str>, &[&str])] = &[
    (
        "SessionStart",
        Some("startup|resume|clear|fork"),
        &[
            "waymux hook tmux SessionStart claude",
            "waymux hook lazygit SessionStart",
        ],
    ),
    (
        "UserPromptSubmit",
        None,
        &[
            "waymux hook tmux UserPromptSubmit claude",
            "waymux hook lazygit UserPromptSubmit",
        ],
    ),
    ("PreToolUse", None, &["waymux hook tmux PreToolUse"]),
    ("PostToolUse", None, &["waymux hook tmux PostToolUse"]),
    (
        "Notification",
        Some("permission_prompt|elicitation_dialog"),
        &["waymux hook tmux Notification claude"],
    ),
    ("Stop", None, &["waymux hook tmux Stop"]),
    (
        "SessionEnd",
        None,
        &[
            "waymux hook tmux SessionEnd",
            "waymux hook lazygit SessionEnd",
        ],
    ),
];

fn claude_hooks_json() -> serde_json::Value {
    let mut hooks = serde_json::Map::new();
    for (event, matcher, commands) in CLAUDE_HOOKS {
        let mut group = serde_json::Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), (*m).into());
        }
        group.insert(
            "hooks".into(),
            commands
                .iter()
                .map(|c| serde_json::json!({ "type": "command", "command": c }))
                .collect(),
        );
        hooks.insert(
            (*event).into(),
            serde_json::Value::Array(vec![group.into()]),
        );
    }
    serde_json::json!({ "hooks": hooks })
}

fn print_hooks() {
    let json = serde_json::to_string_pretty(&claude_hooks_json()).unwrap_or_default();
    println!(
        "# Claude Code: merge into ~/.claude/settings.json (or a project's .claude/settings.json).\n\
{json}\n\n\
# Other agents call the same commands from their own hook config:\n\
#   Antigravity  ~/.gemini/antigravity-cli/hooks.json   waymux hook tmux <Event>\n\
#   Devin        ~/.config/devin/config.json            waymux hook tmux <Event>\n\
# Events: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Notification,\n\
#         PermissionRequest, Stop, SessionEnd. Add `waymux hook lazygit <Event>` on\n\
#         SessionStart/SessionEnd to link review notes to the session.\n\
# Verify with: waymux doctor"
    );
}

pub fn run_cli(args: &[String]) -> i32 {
    let flag = args.first().map(|s| s.as_str()).unwrap_or("--help");
    match flag {
        "--tmux" | "tmux" => {
            println!("{TMUX_SNIPPET}");
            0
        }
        "--systemd" | "systemd" => {
            print!("{SYSTEMD_UNIT}");
            0
        }
        "--hooks" | "hooks" => {
            print_hooks();
            0
        }
        "-h" | "--help" | "help" => {
            print_usage();
            0
        }
        other => {
            eprintln!("waymux init: unknown option '{other}'\n");
            print_usage();
            2
        }
    }
}

fn print_usage() {
    println!(
        "waymux init — print configuration snippets\n\n\
USAGE:\n    \
waymux init --tmux       tmux plugin line and the bindings it sets\n    \
waymux init --systemd    user systemd unit for the waymux daemon\n    \
waymux init --hooks      Claude Code hooks JSON and notes for other agents"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_hooks_cover_waiting_and_finishing() {
        let json = claude_hooks_json();
        let hooks = json["hooks"].as_object().unwrap();
        for event in [
            "SessionStart",
            "PreToolUse",
            "Notification",
            "Stop",
            "SessionEnd",
        ] {
            let cmd = hooks[event][0]["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.starts_with("waymux hook tmux "), "{event}: {cmd}");
        }
        assert_eq!(
            hooks["Notification"][0]["matcher"],
            "permission_prompt|elicitation_dialog"
        );
    }

    #[test]
    fn test_systemd_unit_is_the_shipped_file() {
        assert!(SYSTEMD_UNIT.contains("Type=notify"));
        assert!(SYSTEMD_UNIT.contains("ExecStart=%h/.local/bin/waymux daemon"));
    }
}
