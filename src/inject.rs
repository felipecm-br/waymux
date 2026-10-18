use crate::tmux_cmd::{tmux_load_and_paste_bracketed, tmux_out, tmux_status};
use std::fs::{self, OpenOptions};
use std::io::Write;

fn log_injector(msg: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/tmux-injector.log")
    {
        let _ = writeln!(f, "{}", msg);
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

fn find_target_pane(workspace_dir: &str) -> Option<String> {
    // 1. Check TMUX_ORIGIN_PANE if provided
    if let Ok(orig) = std::env::var("TMUX_ORIGIN_PANE") {
        let orig = orig.trim();
        if !orig.is_empty()
            && let Some(cmd) = tmux_out(&[
                "display-message",
                "-p",
                "-t",
                orig,
                "#{pane_current_command}",
            ])
        {
            let cmd_trim = cmd.trim();
            if is_agent_command(cmd_trim) || (!is_editor_command(cmd_trim) && !cmd_trim.is_empty())
            {
                return Some(orig.to_string());
            }
        }
    }

    // 2. Check active pane sentinel file from lifecycle hooks
    let safe_path = sanitize_workspace_key(workspace_dir);
    let pane_file = format!("/tmp/agy-active-pane-{}.txt", safe_path);
    if let Ok(saved) = fs::read_to_string(&pane_file) {
        let saved_pane = saved.trim();
        if !saved_pane.is_empty()
            && let Some(cmd) = tmux_out(&[
                "display-message",
                "-p",
                "-t",
                saved_pane,
                "#{pane_current_command}",
            ])
        {
            let cmd_trim = cmd.trim();
            if is_agent_command(cmd_trim) || (!is_editor_command(cmd_trim) && !cmd_trim.is_empty())
            {
                return Some(saved_pane.to_string());
            }
        }
    }

    // 3. Search target session (TMUX_ORIGIN_SESSION or client current session)
    let target_session = std::env::var("TMUX_ORIGIN_SESSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| tmux_out(&["display-message", "-p", "#{session_name}"]));

    if let Some(ref sess) = target_session {
        let sess = sess.trim();
        if !sess.is_empty()
            && let Some(panes) = tmux_out(&[
                "list-panes",
                "-s",
                "-t",
                sess,
                "-F",
                "#{pane_id}|#{pane_current_path}|#{pane_current_command}|#{@ai_agent_state_raw}|#{@ai_agent_title}",
            ])
        {
            // Priority A: explicit AI agent in session
            for line in panes.lines() {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 5 {
                    let pid = parts[0];
                    let cmd = parts[2];
                    let raw = parts[3];
                    let title = parts[4];
                    if !raw.is_empty() || !title.is_empty() || is_agent_command(cmd) {
                        return Some(pid.to_string());
                    }
                }
            }

            // Priority B: matching workspace path in session (non-editor)
            for line in panes.lines() {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    let pid = parts[0];
                    let path = parts[1];
                    let cmd = parts[2];
                    if !is_editor_command(cmd)
                        && (path.starts_with(workspace_dir) || workspace_dir.starts_with(path))
                    {
                        return Some(pid.to_string());
                    }
                }
            }
        }
    }

    // 4. Global fallback across all sessions for agent with matching workspace path
    if let Some(panes) = tmux_out(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}|#{pane_current_path}|#{pane_current_command}|#{@ai_agent_state_raw}",
    ]) {
        for line in panes.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 4 {
                let pid = parts[0];
                let path = parts[1];
                let cmd = parts[2];
                let raw = parts[3];
                if (!raw.is_empty() || is_agent_command(cmd))
                    && (path.starts_with(workspace_dir) || workspace_dir.starts_with(path))
                {
                    return Some(pid.to_string());
                }
            }
        }
    }

    None
}

pub fn run_cli(args: &[String]) -> i32 {
    let raw_workspace_dir = args.first().map(|s| s.as_str()).unwrap_or("");
    let raw_prompt = args.get(1).map(|s| s.as_str()).unwrap_or("");

    let workspace_dir = raw_workspace_dir.trim_matches(['\'', '"']);
    let prompt = raw_prompt.trim_matches(['\'', '"']);

    log_injector(&format!("{} {}", workspace_dir, prompt));

    if workspace_dir.is_empty() || prompt.is_empty() {
        return 1;
    }

    let pane = match find_target_pane(workspace_dir) {
        Some(p) => p,
        None => {
            log_injector(&format!(
                "no target agent pane found for workspace ({})",
                workspace_dir
            ));
            return 1;
        }
    };

    if tmux_load_and_paste_bracketed(None, &pane, prompt) {
        let _ = tmux_status(&["send-keys", "-t", &pane, "Enter"]);
        let _ = tmux_out(&[
            "display-message",
            &format!("󰊢 Review note dispatched to AI agent in pane {}", pane),
        ]);
        return 0;
    }

    let single_line = prompt.replace(['\n', '\r'], " ");
    let _ = tmux_status(&["send-keys", "-t", &pane, "-l", &single_line]);
    let _ = tmux_status(&["send-keys", "-t", &pane, "Enter"]);
    let _ = tmux_out(&[
        "display-message",
        &format!("󰊢 Review note dispatched to AI agent in pane {}", pane),
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
}
