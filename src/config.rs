use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Spinner {
    pub interval: u64,
    pub frames: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentStateTheme {
    pub icon: String,
    /// Optional override; empty means the built-in semantic color (`status_palette`).
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub active_spinner: String,
    #[serde(default)]
    pub states: HashMap<String, AgentStateTheme>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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

/// `[approvals]`: let the Cockpit's `y`/`n` answer a permission prompt through the harness's
/// `PermissionRequest` hook instead of keystrokes. Off by default; needs the hook registered
/// (`waymux init --hooks`).
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// How long a prompt is held before the harness shows its own (10 to 300 s, default 120).
    pub hold_seconds: Option<u64>,
}

/// `[checkpoints]`: save each agent pane's work tree at the end of every turn (on by
/// default; see `crate::checkpoint`).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CheckpointsConfig {
    #[serde(default = "default_sound_enabled")]
    pub enabled: bool,
    /// Checkpoints kept per pane (1 to 1000, default 50).
    pub keep: Option<usize>,
}

/// `[phone]`: an ntfy alert when a held prompt is still unanswered after `alert_after_seconds`.
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PhoneConfig {
    /// ntfy topic URL, `https://ntfy.sh/<topic>`; the topic name is the only secret.
    pub ntfy: Option<String>,
    /// A file holding the topic URL instead, for a config kept in a public repository.
    pub ntfy_file: Option<String>,
    /// How long a prompt stays held before the alert (5 to 120 s, default 15).
    pub alert_after_seconds: Option<u64>,
    /// The page the alert opens; default `https://<tailscale name>/m`.
    pub url: Option<String>,
}

impl PhoneConfig {
    pub fn alert_after_secs(&self) -> u64 {
        self.alert_after_seconds.unwrap_or(15).clamp(5, 120)
    }

    pub fn has_topic(&self) -> bool {
        self.ntfy.is_some() || self.ntfy_file.is_some()
    }

    /// The topic URL: `ntfy`, else the first line of `ntfy_file` (a leading `~/` expands).
    pub fn topic(&self) -> Option<String> {
        self.ntfy.clone().or_else(|| {
            let path = crate::sys::expand_home(self.ntfy_file.as_deref()?);
            let text = std::fs::read_to_string(path).ok()?;
            text.lines()
                .next()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
        })
    }
}

impl ApprovalsConfig {
    pub fn hold_secs(&self) -> u64 {
        self.hold_seconds.unwrap_or(120).clamp(10, 300)
    }
}

/// `[quota]`: `waymux quota`, the agent quota report and the launch recommendation
/// (see `crate::quota`).
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QuotaConfig {
    /// Commands (run with `sh -c`) that print a JSON array of extra agents to report on.
    #[serde(default)]
    pub providers: Vec<String>,
    /// Agent names in launch preference order; default `claude`, then the providers' agents.
    pub preference: Option<Vec<String>>,
    /// Assumed 5h burn of one new Claude session in %/h before samples exist (default 10).
    pub session_rate: Option<f64>,
    /// Popup `Enter`: shell command run in a new window; `$WAYMUX_QUOTA_AGENT` holds the
    /// recommended agent. Default: start that agent.
    pub launch: Option<String>,
    /// Popup `a`: shell command that prints the command line to run in a new window (empty
    /// output cancels). Without it the popup has no `a` key.
    pub pick: Option<String>,
}

/// `[launch]`: `waymux launch`, the agent × model menu (see `crate::launch`).
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    /// Choice without an argument or an armed one: `menu` (default), `auto`, `shell` or an agent.
    pub default: Option<String>,
    /// Claude Code `--model` aliases offered as rows (default `opus`, `sonnet`, `haiku`).
    pub claude_models: Option<Vec<String>>,
    /// Agent CLIs without quota telemetry offered when installed (default `devin`, `codex`).
    pub extra: Option<Vec<String>>,
    /// Command that starts the pick as `<exec> <agent> [args...]` (default: the agent itself).
    pub exec: Option<String>,
    /// Command that rewrites the rows: a JSON array on stdin, a JSON array on stdout.
    pub rows_filter: Option<String>,
    /// Waymaker preset for the menu instead of the built-in one.
    pub preset: Option<String>,
    /// Preset layered over `preset` when `--pick` shows the menu.
    pub pick_preset: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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
    /// Hook-held approval settings (`[approvals]`).
    pub approvals: Option<ApprovalsConfig>,
    /// Phone alerts for held prompts (`[phone]`).
    pub phone: Option<PhoneConfig>,
    /// Program/agent name -> Nerd Font glyph, merged over the built-ins in `icons`.
    pub icons: Option<HashMap<String, String>>,
    /// Program run as `<hook> <event> <pane> <extra>` on `agent_question`, `human_reply` and
    /// `agent_finished`. A leading `~/` expands to `$HOME`.
    pub flow_log_hook: Option<String>,
    /// Whether `waymux keylog` appends key presses to `$XDG_STATE_HOME/flow/keys.tsv`.
    #[serde(default)]
    pub keylog: bool,
    pub checkpoints: Option<CheckpointsConfig>,
    /// Agent quota report and launch recommendation (`[quota]`).
    pub quota: Option<QuotaConfig>,
    /// Agent × model launch menu (`[launch]`).
    pub launch: Option<LaunchConfig>,
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
            approvals: None,
            phone: None,
            icons: None,
            flow_log_hook: None,
            keylog: false,
            checkpoints: None,
            quota: None,
            launch: None,
        }
    }
}

impl Config {
    /// The theme's spinner name (`arc` without a theme) and its frames, when defined.
    pub fn active_spinner(&self) -> (String, Option<Spinner>) {
        let name = self
            .theme
            .as_ref()
            .map_or_else(|| "arc".to_string(), |t| t.active_spinner.clone());
        let spinner = self.spinners.as_ref().and_then(|s| s.get(&name).cloned());
        (name, spinner)
    }

    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// The config at `explicit` (`--config`), else at the first default path, else the
    /// built-in defaults, with the path it came from.
    pub fn resolve(explicit: Option<&str>) -> anyhow::Result<(Self, Option<String>)> {
        match explicit.map(str::to_string).or_else(Self::default_path) {
            Some(path) => Ok((
                Self::load(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?,
                Some(path),
            )),
            None => Ok((Self::default(), None)),
        }
    }

    /// The first existing default config path, in the order the daemon searches.
    pub fn default_path() -> Option<String> {
        let xdg_config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            format!("{home}/.config")
        });
        [
            format!("{xdg_config}/waymux/config.toml"),
            format!("{xdg_config}/acpd/config.toml"),
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
    fn test_resolve_names_the_broken_file_and_settings_expand_home() {
        let dir = std::env::temp_dir().join(format!("waymux-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "flow_log_hook = \"/bin/true\"\nidle_debounce_ms = 7\n",
        )
        .unwrap();
        let (config, from) = Config::resolve(path.to_str()).unwrap();
        assert_eq!(from.as_deref(), path.to_str());
        let settings = crate::api::Settings::from_config(&config);
        assert_eq!(settings.idle_debounce_ms, 7);
        assert_eq!(settings.flow_log_hook.as_deref(), Some("/bin/true"));

        std::fs::write(&path, "port = \"not a number\"").unwrap();
        let err = Config::resolve(path.to_str()).unwrap_err().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("config.toml"), "{err}");
    }

    #[test]
    fn test_phone_topic_from_the_config_or_a_file() {
        let dir = std::env::temp_dir().join(format!("waymux-ntfy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ntfy-topic");
        std::fs::write(&file, "  https://ntfy.sh/secret  \nignored\n").unwrap();
        let from_file = PhoneConfig {
            ntfy_file: Some(file.to_string_lossy().into_owned()),
            ..PhoneConfig::default()
        };
        assert!(from_file.has_topic());
        assert_eq!(from_file.topic().as_deref(), Some("https://ntfy.sh/secret"));
        let inline = PhoneConfig {
            ntfy: Some("https://ntfy.sh/inline".into()),
            ..from_file.clone()
        };
        assert_eq!(inline.topic().as_deref(), Some("https://ntfy.sh/inline"));
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(from_file.topic(), None);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!PhoneConfig::default().has_topic());
        assert_eq!(PhoneConfig::default().alert_after_secs(), 15);
    }

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
            r#"
            [theme]
            active_spinner = "minidot"

            [theme.states.permission]
            icon = "󰳌"
        "#,
        )
        .unwrap();
        let theme = config.theme.unwrap();
        let permission = theme.states.get("permission").unwrap();
        assert_eq!(permission.icon, "󰳌");
        assert_eq!(permission.color, "");
    }

    #[test]
    fn test_reference_config_parses() {
        let config = Config::load(concat!(env!("CARGO_MANIFEST_DIR"), "/config/default.toml"));
        assert!(config.is_ok(), "{:?}", config.err());
    }

    #[test]
    fn test_unknown_config_keys_are_rejected() {
        for (bad, key) in [
            ("flow_log_hok = \"/bin/true\"\n", "flow_log_hok"),
            ("[theme]\nactive_spinner = \"dot\"\nextra = 1\n", "extra"),
            (
                "[theme]\nactive_spinner = \"dot\"\n[theme.states.idle]\nicon = \"i\"\ncolr = \"#fff\"\n",
                "colr",
            ),
            ("[sound]\nvol = 50\n", "vol"),
        ] {
            let err = toml::from_str::<Config>(bad).unwrap_err().to_string();
            assert!(
                err.contains("unknown field") && err.contains(key),
                "expected unknown field {key:?}, got: {err}"
            );
        }
    }
}
