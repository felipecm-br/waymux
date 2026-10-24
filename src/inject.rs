use crate::tmux_cmd::{tmux_load_and_paste_bracketed, tmux_out, tmux_status};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Appends to `injector.log` in the private runtime dir: the log holds prompt text, so it
/// must not be readable by other users.
fn log_injector(msg: &str) {
    let Some(dir) = crate::presets::private_runtime_dir() else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("injector.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn sanitize_workspace_key(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn is_agent_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "agy"
            | "agy-bin"
            | "antigravity"
            | "opencode"
            | "claude"
            | "codex"
            | "devin"
            | "muse"
            | "gemini"
            | "aider"
    )
}

fn is_editor_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "nvim"
            | "vim"
            | "vi"
            | "nano"
            | "less"
            | "man"
            | "top"
            | "htop"
            | "btop"
            | "lazygitrs"
            | "lazygit"
    )
}

fn is_shell_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "nu"
            | "elvish"
            | "xonsh"
    )
}

/// Whether a pane may receive the prompt. The prompt is pasted and then submitted with Enter,
/// so a shell would run review-note text as commands: only agents (by command or by the state
/// the hooks record) and programs that are neither shells nor editors qualify; agents often
/// run under `node` or `python`.
fn accepts_prompt(cmd: &str, agent_state: &str) -> bool {
    let cmd = cmd.trim();
    !agent_state.trim().is_empty()
        || is_agent_command(cmd)
        || (!cmd.is_empty() && !is_editor_command(cmd) && !is_shell_command(cmd))
}

/// The pane's directory is the workspace or inside it, by whole path components:
/// `/src/api` is not inside `/src/ap`.
fn in_workspace(pane_path: &str, workspace: &str) -> bool {
    !pane_path.is_empty() && !workspace.is_empty() && Path::new(pane_path).starts_with(workspace)
}

/// Fields of a `list-panes` line in [`PANE_FORMAT`]: id, path, command, agent state, title.
const PANE_FORMAT: &str = "#{pane_id}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{@ai_agent_state_raw}\x1f#{@ai_agent_title}";

fn pane_fields(line: &str) -> Option<[&str; 5]> {
    let mut it = line.split('\x1f');
    let fields = [it.next()?, it.next()?, it.next()?, it.next()?, it.next()?];
    (!fields[0].is_empty()).then_some(fields)
}

/// In the target session: the first pane known to run an agent, else the first pane in the
/// workspace that may receive a prompt.
fn choose_in_session(panes: &str, workspace: &str) -> Option<String> {
    let rows: Vec<[&str; 5]> = panes.lines().filter_map(pane_fields).collect();
    rows.iter()
        .find(|[_, _, cmd, state, title]| {
            !state.is_empty() || !title.is_empty() || is_agent_command(cmd)
        })
        .or_else(|| {
            rows.iter().find(|[_, path, cmd, state, _]| {
                in_workspace(path, workspace) && accepts_prompt(cmd, state)
            })
        })
        .map(|[id, ..]| id.to_string())
}

/// Across all sessions: an agent pane in the workspace.
fn choose_global(panes: &str, workspace: &str) -> Option<String> {
    panes
        .lines()
        .filter_map(pane_fields)
        .find(|[_, path, cmd, state, _]| {
            (!state.is_empty() || is_agent_command(cmd)) && in_workspace(path, workspace)
        })
        .map(|[id, ..]| id.to_string())
}

/// `pane` if it still exists and may receive a prompt.
fn usable_pane(pane: &str) -> Option<String> {
    let pane = pane.trim();
    if pane.is_empty() {
        return None;
    }
    let info = tmux_out(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_current_command}\x1f#{@ai_agent_state_raw}",
    ])?;
    let (cmd, state) = info.split_once('\x1f').unwrap_or((info.as_str(), ""));
    accepts_prompt(cmd, state).then(|| pane.to_string())
}

fn find_target_pane(workspace_dir: &str) -> Option<String> {
    // 1. The pane the Cockpit or lazygitrs was opened from.
    if let Some(pane) = std::env::var("TMUX_ORIGIN_PANE")
        .ok()
        .and_then(|p| usable_pane(&p))
    {
        return Some(pane);
    }

    // 2. The pane the agent hooks recorded for this workspace.
    if let Some(pane) = crate::presets::active_pane_file(workspace_dir)
        .and_then(|f| fs::read_to_string(f).ok())
        .and_then(|p| usable_pane(&p))
    {
        return Some(pane);
    }

    // 3. The target session (TMUX_ORIGIN_SESSION or the client's current one).
    let target_session = std::env::var("TMUX_ORIGIN_SESSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| tmux_out(&["display-message", "-p", "#{session_name}"]));
    if let Some(sess) = target_session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(panes) = tmux_out(&["list-panes", "-s", "-t", sess, "-F", PANE_FORMAT])
        && let Some(pane) = choose_in_session(&panes, workspace_dir)
    {
        return Some(pane);
    }

    // 4. Any session.
    let panes = tmux_out(&["list-panes", "-a", "-F", PANE_FORMAT])?;
    choose_global(&panes, workspace_dir)
}

pub fn run_cli(args: &[String]) -> i32 {
    let raw_workspace_dir = args.first().map_or("", std::string::String::as_str);
    let raw_prompt = args.get(1).map_or("", std::string::String::as_str);

    let workspace_dir = raw_workspace_dir.trim_matches(['\'', '"']);
    let prompt = raw_prompt.trim_matches(['\'', '"']);

    log_injector(&format!("{workspace_dir} {prompt}"));

    if workspace_dir.is_empty() || prompt.is_empty() {
        return 1;
    }

    let Some(pane) = find_target_pane(workspace_dir) else {
        log_injector(&format!(
            "no target agent pane found for workspace ({workspace_dir})"
        ));
        return 1;
    };

    if tmux_load_and_paste_bracketed(None, &pane, prompt) {
        let _ = tmux_status(&["send-keys", "-t", &pane, "Enter"]);
        let _ = tmux_out(&[
            "display-message",
            &format!("󰊢 Review note dispatched to AI agent in pane {pane}"),
        ]);
        return 0;
    }

    let single_line = prompt.replace(['\n', '\r'], " ");
    let _ = tmux_status(&["send-keys", "-t", &pane, "-l", &single_line]);
    let _ = tmux_status(&["send-keys", "-t", &pane, "Enter"]);
    let _ = tmux_out(&[
        "display-message",
        &format!("󰊢 Review note dispatched to AI agent in pane {pane}"),
    ]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_workspace_key() {
        assert_eq!(
            sanitize_workspace_key("/home/fecavmi/.dotfiles/main"),
            "_home_fecavmi__dotfiles_main"
        );
    }

    #[test]
    fn test_accepts_prompt_never_picks_a_shell() {
        assert!(accepts_prompt("claude", ""));
        assert!(accepts_prompt("node", ""), "agents often run under node");
        assert!(accepts_prompt("zsh", "idle"), "a recorded agent state wins");
        for shell in ["zsh", "bash", "fish", "sh"] {
            assert!(!accepts_prompt(shell, ""), "{shell} would run the prompt");
        }
        assert!(!accepts_prompt("nvim", ""));
        assert!(!accepts_prompt("", ""));
    }

    #[test]
    fn test_in_workspace_matches_whole_components() {
        assert!(in_workspace("/src/api", "/src/api"));
        assert!(in_workspace("/src/api/crates/x", "/src/api"));
        assert!(in_workspace("/src/api", "/src/api/"));
        assert!(!in_workspace("/src/api-old", "/src/api"));
        assert!(
            !in_workspace("/", "/src/api"),
            "a parent directory is not the workspace"
        );
        assert!(!in_workspace("", "/src/api"));
        assert!(!in_workspace("/src/api", ""));
    }

    fn panes(rows: &[[&str; 5]]) -> String {
        rows.iter()
            .map(|r| r.join("\x1f"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_choose_in_session_prefers_agents_then_workspace() {
        let session = panes(&[
            ["%1", "/src/api", "zsh", "", ""],
            ["%2", "/src/api", "node", "", ""],
            ["%3", "/elsewhere", "claude", "", ""],
        ]);
        assert_eq!(
            choose_in_session(&session, "/src/api").as_deref(),
            Some("%3")
        );

        let no_agent = panes(&[
            ["%1", "/src/api", "zsh", "", ""],
            ["%2", "/src/api-old", "node", "", ""],
            ["%4", "/src/api/web", "node", "", ""],
        ]);
        assert_eq!(
            choose_in_session(&no_agent, "/src/api").as_deref(),
            Some("%4")
        );
        let shells_only = panes(&[["%1", "/src/api", "zsh", "", ""]]);
        assert_eq!(choose_in_session(&shells_only, "/src/api"), None);
        assert_eq!(choose_in_session("%1|/src/api|claude||", "/src/api"), None);
    }

    #[test]
    fn test_choose_global_needs_an_agent_in_the_workspace() {
        let all = panes(&[
            ["%1", "/", "claude", "", ""],
            ["%2", "/src/api-old", "claude", "", ""],
            ["%3", "/src/api", "node", "", ""],
            ["%4", "/src/api", "node", "working", ""],
        ]);
        assert_eq!(choose_global(&all, "/src/api").as_deref(), Some("%4"));
        assert_eq!(choose_global(&all, "/nowhere"), None);
    }
}
