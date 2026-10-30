use std::fs;
use std::path::PathBuf;

pub const COCKPIT_PICKER_TOML_TEMPLATE: &str = include_str!("cockpit-picker.toml");
pub const LAUNCH_PICKER_TOML_TEMPLATE: &str = include_str!("launch-picker.toml");

fn runtime_dir_from(xdg: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(xdg) = xdg.filter(|v| !v.is_empty()) {
        PathBuf::from(xdg).join("waymux")
    } else {
        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/waymux-{uid}"))
    }
}

pub fn runtime_dir() -> PathBuf {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    runtime_dir_from(xdg.as_deref())
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
        .map_or_else(|| "waymux".to_string(), |p| p.to_string_lossy().to_string());

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

/// Fills the launch menu preset. `esc_label` names what `Esc` does: `Shell` in a pane that
/// becomes a shell, `Close` when a caller (a popup) closes.
pub fn render_launch_toml(muted_text: &str, muted_line: &str, esc_label: &str) -> String {
    LAUNCH_PICKER_TOML_TEMPLATE
        .replace("__MUTED_TEXT__", muted_text)
        .replace("__MUTED_LINE__", muted_line)
        .replace("__ESC_LABEL__", esc_label)
}

/// Writes the launch menu preset to the runtime dir (only when it changed) and returns its path.
pub fn materialize_launch_toml(esc_label: &str) -> PathBuf {
    let dir = runtime_dir();
    let _ = fs::create_dir_all(&dir);
    let target = dir.join(format!(
        "launch-picker-{}.toml",
        esc_label.to_ascii_lowercase()
    ));
    let theme = crate::theme::ThemePalette::load();
    let rendered = render_launch_toml(&theme.muted, &theme.muted_line, esc_label);
    if fs::read_to_string(&target).ok().as_deref() != Some(rendered.as_str()) {
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

    #[test]
    fn test_launch_preset_is_filled_and_valid() {
        let toml = render_launch_toml("#7f9bb3", "#5d7a92", "Close");
        assert!(!toml.contains("__"), "unfilled placeholder");
        assert!(!toml.contains("\"DIM\""), "rows must not be dimmed");
        assert!(!toml.to_ascii_lowercase().contains("darkgray"));
        assert!(toml.contains("{#7f9bb3:Close}"));
        assert!(toml.parse::<toml::Table>().is_ok());
    }

    #[test]
    fn test_runtime_dir_prefers_xdg_and_falls_back_to_uid_tmp() {
        assert_eq!(
            runtime_dir_from(Some(std::ffi::OsStr::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000/waymux")
        );
        let fallback = runtime_dir_from(None);
        assert!(
            fallback.to_string_lossy().starts_with("/tmp/waymux-"),
            "{fallback:?}"
        );
        assert!(active_pane_file("/repo/a").is_some());
    }
}
