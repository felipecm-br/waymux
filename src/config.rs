use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
pub struct Spinner {
    pub interval: u64,
    pub frames: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentStateTheme {
    pub icon: String,
    /// Optional override; empty means the built-in semantic color (`status_palette`).
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ThemeConfig {
    pub active_spinner: String,
    #[serde(default)]
    pub states: HashMap<String, AgentStateTheme>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SoundConfig {
    #[serde(default = "default_sound_enabled")]
    pub enabled: bool,
    pub player: Option<String>,
    pub response: Option<String>,
    pub question: Option<String>,
    pub permission: Option<String>,
    pub error: Option<String>,
}

fn default_sound_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Config {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub pid_file: Option<String>,
    pub shutdown_timeout_secs: Option<u64>,
    pub log_level: Option<String>,
    pub idle_debounce_ms: Option<u64>,
    pub theme: Option<ThemeConfig>,
    pub spinners: Option<HashMap<String, Spinner>>,
    pub sound: Option<SoundConfig>,
    /// Program/agent name -> Nerd Font glyph, merged over the built-ins in `icons`.
    pub icons: Option<HashMap<String, String>>,
}

fn default_listen_addr() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    4040
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            port: default_port(),
            pid_file: None,
            shutdown_timeout_secs: Some(5),
            log_level: Some("info".to_string()),
            idle_debounce_ms: Some(300),
            theme: None,
            spinners: None,
            sound: Some(SoundConfig {
                enabled: true,
                player: None,
                response: None,
                question: None,
                permission: None,
                error: None,
            }),
            icons: None,
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// The first existing default config path, in the order the daemon searches.
    pub fn default_path() -> Option<String> {
        let xdg_config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            format!("{}/.config", home)
        });
        [
            format!("{}/waymux/config.toml", xdg_config),
            format!("{}/acpd/config.toml", xdg_config),
            "/etc/waymux/config.toml".to_string(),
            "/etc/acpd/config.toml".to_string(),
            "config/default.toml".to_string(),
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
    }
}

/// Theme section of the default config, for CLI surfaces (Cockpit) that share the
/// daemon's state colors.
pub fn load_default_theme() -> Option<ThemeConfig> {
    Config::default_path()
        .and_then(|p| Config::load(&p).ok())
        .and_then(|c| c.theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_loading() {
        let toml_content = r##"
            listen_addr = "127.0.0.1"
            port = 4040
            [theme]
            active_spinner = "minidot"

            [theme.states.idle]
            icon = "󰗡"
            color = "#94e2d5"

            [spinners]
            minidot = { interval = 83, frames = ["⠋", "⠙", "⠹"] }
            line = { interval = 100, frames = ["|", "/"] }
        "##;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert_eq!(config.listen_addr, "127.0.0.1");
        assert_eq!(config.port, 4040);
        let theme = config.theme.unwrap();
        assert_eq!(theme.active_spinner, "minidot");
        assert_eq!(theme.states.get("idle").unwrap().color, "#94e2d5");

        let spinners = config.spinners.unwrap();
        let minidot = spinners.get("minidot").unwrap();
        assert_eq!(minidot.interval, 83);
        assert_eq!(
            minidot.frames,
            vec!["⠋".to_string(), "⠙".to_string(), "⠹".to_string()]
        );
    }

    #[test]
    fn test_state_color_is_optional() {
        let config: Config = toml::from_str(
            r##"
            [theme]
            active_spinner = "minidot"

            [theme.states.permission]
            icon = "󰳌"
        "##,
        )
        .unwrap();
        let theme = config.theme.unwrap();
        let permission = theme.states.get("permission").unwrap();
        assert_eq!(permission.icon, "󰳌");
        assert!(permission.color.is_empty());
    }

    #[test]
    fn test_reference_config_parses() {
        let config = Config::load(concat!(env!("CARGO_MANIFEST_DIR"), "/config/default.toml"));
        assert!(config.is_ok(), "{:?}", config.err());
    }
}
