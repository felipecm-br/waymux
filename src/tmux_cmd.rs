use std::io::Write;
use std::process::{Command, Stdio};

/// Socket that unit tests run tmux against. No server ever listens on it, so a test that
/// reaches tmux gets "no server running" instead of changing the developer's session.
#[cfg(test)]
pub const TEST_SOCKET: &str = "waymux-unit-test";

/// Forces UTF-8 output whatever the locale. Without it, a client whose `LC_ALL/LC_CTYPE/LANG`
/// is not UTF-8 (cron, `env -i`, some service managers) turns the tabs in `-F` formats and
/// every non-ASCII glyph into `_`, so tab-separated rows no longer split and agent states
/// and icons are lost.
const UTF8: &str = "-u";

/// A `tmux` command. Every call site builds tmux through here (or [`async_command`]).
pub fn command() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.arg(UTF8);
    #[cfg(test)]
    cmd.args(["-L", TEST_SOCKET]);
    cmd
}

/// Async counterpart of [`command`].
pub fn async_command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.arg(UTF8);
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

/// `tmux_out` for a format whose fields are split by 0x1f (unit separator).
pub fn tmux_fields_out(args: &[&str]) -> Option<String> {
    tmux_out(args).map(decode_fields)
}

/// tmux 3.4 (Ubuntu 24.04, Debian) escapes `-F` and `display -p` output: 0x1f comes out as
/// the four characters `\037` and a backslash as `\\`, so a line has no field to split.
/// Newer tmux prints both raw. Decoding runs only when no raw separator is present, so a path
/// holding a literal `\037` stays as it is on a tmux that does not escape.
pub fn decode_fields(out: String) -> String {
    if out.contains('\x1f') || !out.contains("\\037") {
        return out;
    }
    let mut decoded = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(i) = rest.find('\\') {
        decoded.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let octal = tail
            .get(..3)
            .filter(|d| d.bytes().all(|b| (b'0'..=b'7').contains(&b)));
        if let Some(c) = octal
            .and_then(|d| u8::from_str_radix(d, 8).ok())
            .filter(u8::is_ascii)
        {
            decoded.push(char::from(c));
            rest = &tail[3..];
        } else if let Some(stripped) = tail.strip_prefix('\\') {
            decoded.push('\\');
            rest = stripped;
        } else {
            decoded.push('\\');
            rest = tail;
        }
    }
    decoded.push_str(rest);
    decoded
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
        .is_ok_and(|s| s.success())
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

    let Ok(mut child) = command()
        .args(&load_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(text.as_bytes()).is_err()
    {
        return false;
    }

    if !child.wait().is_ok_and(|s| s.success()) {
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
mod decode_fields_tests {
    use super::decode_fields;

    #[test]
    fn tmux_34_escaped_output_splits_again() {
        let escaped = "ma\\\\in\\037%0\\037/tmp/a b\nmain\\0371\\037zsh".to_string();
        assert_eq!(
            decode_fields(escaped),
            "ma\\in\x1f%0\x1f/tmp/a b\nmain\x1f1\x1fzsh"
        );
    }

    #[test]
    fn raw_output_is_left_alone() {
        let raw = "main\x1f%0\x1f/tmp/odd\\037name".to_string();
        assert_eq!(decode_fields(raw.clone()), raw);
        assert_eq!(decode_fields("C:\\path".to_string()), "C:\\path");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_use_private_socket_under_test() {
        let sync_args: Vec<_> = command()
            .get_args()
            .map(std::borrow::ToOwned::to_owned)
            .collect();
        assert_eq!(sync_args, [UTF8, "-L", TEST_SOCKET]);
        let async_args: Vec<_> = async_command()
            .as_std()
            .get_args()
            .map(std::borrow::ToOwned::to_owned)
            .collect();
        assert_eq!(async_args, [UTF8, "-L", TEST_SOCKET]);
    }
}
