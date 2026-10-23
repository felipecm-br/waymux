use std::fs;
use std::path::PathBuf;

pub const COCKPIT_PICKER_TOML_TEMPLATE: &str = include_str!("cockpit-picker.toml");

pub fn runtime_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg).join("waymux")
    } else {
        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/waymux-{}", uid))
    }
}

/// [`runtime_dir`], created `0700` when missing. `None` when it is not a directory owned by the
/// current user: the `/tmp/waymux-<uid>` fallback can be created by anyone first, and files
/// placed there (which pane gets a prompt, the PID file, caches) must not be theirs to choose.
pub fn private_runtime_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let dir = runtime_dir();
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() || meta.uid() != nix::unistd::getuid().as_raw() {
        return None;
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

/// File recording the tmux pane of the agent working in `workspace`, written by the hooks and
/// read by `waymux inject`.
pub fn active_pane_file(workspace: &str) -> Option<PathBuf> {
    Some(private_runtime_dir()?.join(format!(
        "active-pane-{}",
        crate::inject::sanitize_workspace_key(workspace)
    )))
}

/// Fills the preset template: the waymux binary path, and the metadata text and line colors
/// (theme `muted` lifted to WCAG contrast; ANSI darkgray is 1.68:1 under lumon).
pub fn render_cockpit_toml(waymux_bin: &str, muted_text: &str, muted_line: &str) -> String {
    COCKPIT_PICKER_TOML_TEMPLATE
        .replace("__WAYMUX_BIN__", waymux_bin)
        .replace("__MUTED_TEXT__", muted_text)
        .replace("__MUTED_LINE__", muted_line)
}

pub fn materialize_cockpit_toml() -> PathBuf {
    let dir = runtime_dir();
    let _ = fs::create_dir_all(&dir);
    let target = dir.join("cockpit-picker.toml");

    let exe_path = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "waymux".to_string());

    let theme = crate::theme::ThemePalette::load();
    let rendered = render_cockpit_toml(&exe_path, &theme.muted, &theme.muted_line);

    let needs_write = match fs::read_to_string(&target) {
        Ok(existing) => existing != rendered,
        Err(_) => true,
    };

    if needs_write {
        let _ = fs::write(&target, rendered);
    }

    target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rendered_preset_has_no_placeholders_dim_rows_or_darkgray() {
        let toml = render_cockpit_toml("/bin/waymux", "#7f9bb3", "#5d7a92");
        for placeholder in ["__WAYMUX_BIN__", "__MUTED_TEXT__", "__MUTED_LINE__"] {
            assert!(!toml.contains(placeholder), "unfilled {placeholder}");
        }
        assert!(!toml.contains("\"DIM\""), "rows must not be dimmed");
        assert!(!toml.to_ascii_lowercase().contains("darkgray"));
        assert!(toml.contains("{#7f9bb3:Jump}"));
        assert!(toml.parse::<toml::Table>().is_ok());
    }
}
