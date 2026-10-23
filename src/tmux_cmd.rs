use std::io::Write;
use std::process::{Command, Stdio};

/// Socket that unit tests run tmux against. No server ever listens on it, so a test that
/// reaches tmux gets "no server running" instead of changing the developer's session.
#[cfg(test)]
pub const TEST_SOCKET: &str = "waymux-unit-test";

/// A `tmux` command. Every call site builds tmux through here (or [`async_command`]).
pub fn command() -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new("tmux");
    #[cfg(test)]
    cmd.args(["-L", TEST_SOCKET]);
    cmd
}

/// Async counterpart of [`command`].
pub fn async_command() -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new("tmux");
    #[cfg(test)]
    cmd.args(["-L", TEST_SOCKET]);
    cmd
}

pub fn tmux_out(args: &[&str]) -> Option<String> {
    let output = command().args(args).stderr(Stdio::null()).output().ok()?;
    if output.status.success() {
        Some(
            String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        )
    } else {
        None
    }
}

pub fn tmux_out_raw(args: &[&str]) -> Option<String> {
    let output = command().args(args).stderr(Stdio::null()).output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

pub fn tmux_status(args: &[&str]) -> bool {
    command()
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn tmux_get_global_opt(opt: &str) -> String {
    tmux_out(&["show-option", "-gqv", opt]).unwrap_or_default()
}

pub fn tmux_set_global_opt(opt: &str, val: &str) {
    let _ = tmux_status(&["set-option", "-g", opt, val]);
}

pub fn tmux_unset_global_opt(opt: &str) {
    let _ = tmux_status(&["set-option", "-gu", opt]);
}

pub fn tmux_display_msg(msg: &str) {
    let _ = tmux_status(&["display-message", msg]);
}

pub fn tmux_display_msg_timed(ms: u32, msg: &str) {
    let ms_str = ms.to_string();
    let _ = tmux_status(&["display-message", "-d", &ms_str, msg]);
}

pub fn tmux_load_and_paste_bracketed(
    buf_name: Option<&str>,
    target_pane: &str,
    text: &str,
) -> bool {
    let mut load_args = vec!["load-buffer"];
    if let Some(b) = buf_name {
        load_args.push("-b");
        load_args.push(b);
    }
    load_args.push("-");

    let mut child = match command()
        .args(&load_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(text.as_bytes()).is_err()
    {
        return false;
    }

    if !child.wait().map(|s| s.success()).unwrap_or(false) {
        return false;
    }

    let mut paste_args = vec!["paste-buffer"];
    if let Some(b) = buf_name {
        paste_args.push("-b");
        paste_args.push(b);
    }
    paste_args.extend(["-t", target_pane, "-p"]);

    if tmux_status(&paste_args) {
        let _ = tmux_status(&["send-keys", "-t", target_pane, "Enter"]);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_use_private_socket_under_test() {
        let sync_args: Vec<_> = command().get_args().map(|a| a.to_owned()).collect();
        assert_eq!(sync_args, ["-L", TEST_SOCKET]);
        let async_args: Vec<_> = async_command()
            .as_std()
            .get_args()
            .map(|a| a.to_owned())
            .collect();
        assert_eq!(async_args, ["-L", TEST_SOCKET]);
    }
}
