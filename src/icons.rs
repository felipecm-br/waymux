//! Nerd Font icon registry: the one place that maps program/agent names and agent states to
//! glyphs. Built-in defaults are overridden by `[icons]` (programs) and `[theme.states.<key>]`
//! (states) in the config. Everything else reads glyphs from here: the hook's window rename,
//! the Cockpit, the tmux/waybar state badges, and shell or tmux consumers through `waymux icons`.

use crate::config::Config;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

/// Generic glyph for an agent whose name has no entry of its own.
pub const AGENT: &str = "agent";

const DEFAULT_PROGRAMS: &[(&str, &str)] = &[
    (AGENT, "󰚩"),
    ("agy", ""),
    ("agy-bin", ""),
    ("antigravity", ""),
    ("claude", ""),
    ("codex", ""),
    ("devin", ""),
    ("muse", "󱚣"),
    ("opencode", "󰅪"),
];

const DEFAULT_STATES: &[(&str, &str)] = &[
    ("idle", "󱥂"),
    ("busy", "󰑮"),
    ("working", "󰑮"),
    ("question", "󱜻"),
    ("permission", "󱅭"),
    ("stalled", "󱥁"),
    ("error", "󰨄"),
    ("review", "󰷋"),
];

/// Aliases share their canonical state's glyph, so one override in the config covers both.
fn canonical_state(state: &str) -> &str {
    match state {
        "awaiting_input" => "question",
        "hanging" => "stalled",
        other => other,
    }
}

pub fn default_state(state: &str) -> Option<&'static str> {
    let key = canonical_state(state);
    DEFAULT_STATES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}

pub struct Registry {
    programs: BTreeMap<String, String>,
    states: BTreeMap<String, String>,
}

impl Registry {
    pub fn builtin() -> Self {
        let collect = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        Self {
            programs: collect(DEFAULT_PROGRAMS),
            states: collect(DEFAULT_STATES),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        let mut reg = Self::builtin();
        for (name, glyph) in config.icons.iter().flatten() {
            if !glyph.is_empty() {
                reg.programs
                    .insert(name.to_ascii_lowercase(), glyph.clone());
            }
        }
        for (state, theme) in config.theme.iter().flat_map(|t| &t.states) {
            if !theme.icon.is_empty() {
                reg.states.insert(state.clone(), theme.icon.clone());
            }
        }
        reg
    }

    /// The registry of the default config, read once per process.
    pub fn global() -> &'static Registry {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            Config::default_path()
                .and_then(|p| Config::load(&p).ok())
                .map_or_else(Self::builtin, |c| Self::from_config(&c))
        })
    }

    pub fn program(&self, name: &str) -> Option<&str> {
        self.programs
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The agent's own glyph, else the generic agent glyph.
    pub fn agent(&self, name: &str) -> &str {
        self.program(name)
            .or_else(|| self.program(AGENT))
            .unwrap_or_default()
    }

    pub fn state(&self, state: &str) -> Option<&str> {
        self.states.get(canonical_state(state)).map(String::as_str)
    }
}

const USAGE: &str = "usage: waymux icon <name> | waymux icon --state <state>
       waymux icons [--format tsv|sh|tmux]";

/// `waymux icon <name>` / `waymux icon --state <state>`: print one glyph, exit 1 when unknown.
pub fn run_icon_cli(args: &[String]) -> i32 {
    let reg = Registry::global();
    let glyph = match args {
        [flag, state] if flag == "--state" => reg.state(state),
        [name] if !name.starts_with('-') => reg.program(name),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match glyph {
        Some(g) => {
            println!("{g}");
            0
        }
        None => 1,
    }
}

/// `waymux icons`: export the merged registry for shell scripts and the tmux window-name plugin.
pub fn run_icons_cli(args: &[String]) -> i32 {
    let format = match args {
        [] => "tsv",
        [flag, f] if flag == "--format" => f.as_str(),
        [f] if f.starts_with("--format=") => &f["--format=".len()..],
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if let Some(out) = render(Registry::global(), format) {
        print!("{out}");
        0
    } else {
        eprintln!("waymux icons: unknown format '{format}' (tsv, sh, tmux)");
        2
    }
}

fn render(reg: &Registry, format: &str) -> Option<String> {
    let mut out = String::new();
    match format {
        "tsv" => {
            for (k, v) in &reg.programs {
                let _ = writeln!(out, "program\t{k}\t{v}");
            }
            for (k, v) in &reg.states {
                let _ = writeln!(out, "state\t{k}\t{v}");
            }
        }
        "sh" => {
            for (k, v) in &reg.programs {
                let _ = writeln!(out, "ICON_{}={}", sh_name(k), sh_quote(v));
            }
            for (k, v) in &reg.states {
                let _ = writeln!(out, "ICON_STATE_{}={}", sh_name(k), sh_quote(v));
            }
        }
        // The `icons:` section of tmux-nerd-font-window-name's YAML config.
        "tmux" => {
            out.push_str("icons:\n");
            for (k, v) in &reg.programs {
                let _ = writeln!(out, "  {k}: \"{}\"", v.replace('"', "\\\""));
            }
        }
        _ => return None,
    }
    Some(out)
}

fn sh_name(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn test_config_overrides_builtin_programs_and_states() {
        let reg = Registry::from_config(&config(
            r#"
            [icons]
            Devin = "D"
            yazi = "Y"
            [theme]
            active_spinner = "dot"
            [theme.states.question]
            icon = "Q"
            "#,
        ));
        assert_eq!(reg.program("devin"), Some("D"));
        assert_eq!(reg.program("yazi"), Some("Y"));
        assert_eq!(reg.program("claude"), Some(""));
        assert_eq!(reg.state("question"), Some("Q"));
        assert_eq!(reg.state("awaiting_input"), Some("Q"));
        assert_eq!(reg.state("hanging"), Some("󱥁"));
    }

    #[test]
    fn test_unknown_agent_gets_generic_glyph() {
        let reg = Registry::builtin();
        assert_eq!(reg.agent("devin"), "");
        assert_eq!(reg.agent("aider"), "󰚩");
        assert_eq!(reg.program("aider"), None);
    }

    #[test]
    fn test_render_formats() {
        let reg = Registry::builtin();
        let sh = render(&reg, "sh").unwrap();
        assert!(sh.contains("ICON_AGY_BIN=''\n"));
        assert!(sh.contains("ICON_STATE_IDLE='󱥂'\n"));
        let tmux = render(&reg, "tmux").unwrap();
        assert!(tmux.starts_with("icons:\n"));
        assert!(tmux.contains("  devin: \"\"\n"));
        assert!(render(&reg, "tsv").unwrap().contains("state\treview\t󰷋\n"));
        assert!(render(&reg, "xml").is_none());
    }

    #[test]
    fn test_sh_quote_escapes_single_quotes() {
        assert_eq!(sh_quote("a'b"), r"'a'\''b'");
    }
}
