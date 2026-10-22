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

/// Audio players in preference order: PipeWire, PulseAudio, ALSA, macOS.
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
    fn test_find_in_path_finds_sh_and_rejects_missing() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("waymux-definitely-not-installed").is_none());
    }
}
