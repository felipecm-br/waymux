use std::path::{Path, PathBuf};

/// First executable named `name` on `$PATH`, like `command -v`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Quote `s` as one POSIX shell word: `it's` becomes `'it'\''s'`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `path` with a leading `~/` replaced by `home`; anything else (`~user/`, absolute) is kept.
pub fn expand_home_in(home: &str, path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    }
}

/// [`expand_home_in`] with `$HOME`; the path is returned unchanged when `$HOME` is unset.
pub fn expand_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) => expand_home_in(&home, path),
        Err(_) => path.to_string(),
    }
}

/// Milliseconds since the Unix epoch; 0 when the clock is before it.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Audio players in preference order: `PipeWire`, `PulseAudio`, ALSA, macOS.
const SOUND_PLAYERS: [&str; 4] = ["pw-play", "paplay", "aplay", "afplay"];

/// The first installed audio player, or `pw-play` so the failure names a real tool.
pub fn default_sound_player() -> String {
    SOUND_PLAYERS
        .iter()
        .find(|p| find_in_path(p).is_some())
        .unwrap_or(&SOUND_PLAYERS[0])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_home_in_only_rewrites_a_leading_tilde_slash() {
        assert_eq!(expand_home_in("/h", "~/a.wav"), "/h/a.wav");
        assert_eq!(expand_home_in("/h", "~user/a.wav"), "~user/a.wav");
        assert_eq!(expand_home_in("/h", "/abs/a.wav"), "/abs/a.wav");
    }

    #[test]
    fn test_shell_quote_survives_sh_round_trip() {
        for word in ["plain", "with space", "it's", "a'b'c", "$(rm -rf ~)", ""] {
            let out = std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {}", shell_quote(word))])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), word);
        }
    }

    #[test]
    fn test_find_in_path_finds_sh_and_rejects_missing() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("waymux-definitely-not-installed").is_none());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// Whatever the value, a real POSIX shell reads the quoted word back unchanged.
            #[test]
            fn shell_quote_round_trips_through_sh(s in "[^\\x00]{0,40}") {
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("printf '%s' {}", shell_quote(&s)))
                    .output()
                    .unwrap();
                prop_assert!(out.status.success());
                prop_assert_eq!(String::from_utf8_lossy(&out.stdout), s);
            }
        }
    }
}
