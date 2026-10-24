pub mod adapters;
pub mod api;
pub mod auth;
pub mod cockpit;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod health;
pub mod hook;
pub mod icons;
pub mod init;
pub mod inject;
pub mod pid;
pub mod presets;
pub mod rpc_client;
pub mod signals;
pub mod status_palette;
pub mod sys;
#[cfg(test)]
pub(crate) mod test_strategies;
pub mod theme;
pub mod tmux_cmd;
pub mod triage;
pub mod wt;

use std::path::Path;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct CommandSpec {
    name: &'static str,
    usage: &'static str,
    about: &'static str,
    /// Called by the Cockpit preset and tmux bindings rather than typed by hand.
    plumbing: bool,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "daemon",
        usage: "daemon [-c <FILE>]",
        about: "Start the JSON-RPC & HTTP state daemon on 127.0.0.1:4040",
        plumbing: false,
    },
    CommandSpec {
        name: "health",
        usage: "health",
        about: "Exit 0 if the daemon answers GET /health",
        plumbing: false,
    },
    CommandSpec {
        name: "status",
        usage: "status",
        about: "Print daemon health and active AI agent states",
        plumbing: false,
    },
    CommandSpec {
        name: "list",
        usage: "list",
        about: "List active AI agent pane states (TSV: pane, state, timestamp)",
        plumbing: false,
    },
    CommandSpec {
        name: "dismiss",
        usage: "dismiss [pane_id]",
        about: "Dismiss the attention badge for a pane (default: $TMUX_PANE)",
        plumbing: false,
    },
    CommandSpec {
        name: "cockpit",
        usage: "cockpit [--agents]",
        about: "Open the Fleet & Agent Cockpit popup (needs Waymaker `wm`)",
        plumbing: false,
    },
    CommandSpec {
        name: "triage",
        usage: "triage [--popup | --jump]",
        about: "Cycle (--popup, default) or jump to (--jump) the next waiting agent",
        plumbing: false,
    },
    CommandSpec {
        name: "review-notes",
        usage: "review-notes <count|list|send> <path> [session] [idx] [pane_id]",
        about: "Count, list, or send lazygitrs review notes to an agent",
        plumbing: false,
    },
    CommandSpec {
        name: "inject",
        usage: "inject <workspace_dir> <prompt>",
        about: "Paste a multi-line prompt into the workspace's agent pane",
        plumbing: false,
    },
    CommandSpec {
        name: "wt",
        usage: "wt <subcommand>",
        about: "Git worktree & multi-agent session orchestrator (also `awt`)",
        plumbing: false,
    },
    CommandSpec {
        name: "hook",
        usage: "hook <tmux|lazygit> <Event> [agent]",
        about: "AI agent lifecycle hook; reads the hook JSON on stdin",
        plumbing: false,
    },
    CommandSpec {
        name: "icon",
        usage: "icon <name> | icon --state <state>",
        about: "Print the Nerd Font glyph for a program/agent or an agent state",
        plumbing: false,
    },
    CommandSpec {
        name: "icons",
        usage: "icons [--format tsv|sh|tmux]",
        about: "Export the icon registry for shell scripts and the tmux window-name plugin",
        plumbing: false,
    },
    CommandSpec {
        name: "init",
        usage: "init <--tmux | --systemd | --hooks>",
        about: "Print configuration snippets for tmux, systemd, and agent hooks",
        plumbing: false,
    },
    CommandSpec {
        name: "doctor",
        usage: "doctor",
        about: "Check the daemon, tools, scripts and theme, and say how to fix gaps",
        plumbing: false,
    },
    CommandSpec {
        name: "cockpit-items",
        usage: "cockpit-items [--agents] [session] [window]",
        about: "Emit the Cockpit's 12-column TSV rows",
        plumbing: true,
    },
    CommandSpec {
        name: "cockpit-preview",
        usage: "cockpit-preview <session> <idx> [pane_id] [path]",
        about: "Render the Cockpit preview card",
        plumbing: true,
    },
    CommandSpec {
        name: "cockpit-diff",
        usage: "cockpit-diff <session> <idx> ...",
        about: "Render the Cockpit diff pane",
        plumbing: true,
    },
    CommandSpec {
        name: "cockpit-help",
        usage: "cockpit-help",
        about: "Render the Cockpit keybinding cheat-sheet",
        plumbing: true,
    },
    CommandSpec {
        name: "agent-action",
        usage: "agent-action <approve|deny|prompt> <state> <pane_id>",
        about: "Approve, deny or prompt an agent from the Cockpit",
        plumbing: true,
    },
    CommandSpec {
        name: "window-create",
        usage: "window-create",
        about: "Create a window next to the Cockpit selection",
        plumbing: true,
    },
    CommandSpec {
        name: "window-delete",
        usage: "window-delete",
        about: "Close the Cockpit selection, guarding working agents",
        plumbing: true,
    },
];

fn find_command(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.name == name)
}

fn print_help(bin_name: &str) {
    println!(
        "{bin_name} {}\nWaymux — Terminal AI Cockpit & Agent Control Plane Daemon\n\n\
USAGE:\n    {bin_name} <COMMAND> [ARGS]\n    {bin_name} <COMMAND> --help\n\n\
OPTIONS:\n    \
-h, --help             Print help information\n    \
-V, --version          Print version information\n\nCOMMANDS:",
        env!("CARGO_PKG_VERSION")
    );
    for c in COMMANDS.iter().filter(|c| !c.plumbing) {
        println!("    {:<16} {}", c.name, c.about);
    }
    println!("\nPLUMBING (called by the Cockpit and tmux bindings):");
    for c in COMMANDS.iter().filter(|c| c.plumbing) {
        println!("    {:<16} {}", c.name, c.about);
    }
}

fn print_command_help(bin_name: &str, c: &CommandSpec) {
    println!("{}\n\nUSAGE:\n    {bin_name} {}", c.about, c.usage);
}

/// Commands whose own parser prints richer help than the shared one-liner.
fn has_own_help(name: &str) -> bool {
    matches!(name, "wt" | "awt" | "init")
}

fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

/// Daemon mode is the bare binary, `daemon`, or a leading `--config` flag
/// (kept for the original `acpd --config <file>` invocation).
fn is_daemon_invocation(first_arg: &str) -> bool {
    first_arg.is_empty()
        || first_arg == "daemon"
        || first_arg == "-c"
        || first_arg == "--config"
        || first_arg.starts_with("--config=")
}

/// Rust ignores SIGPIPE, so `waymux list | head` panics on the closed pipe. Commands that
/// only print get the default disposition back and exit quietly. The daemon and the
/// socket-writing hot paths (`hook`, `inject`, `dismiss`) keep EPIPE as an error.
fn restore_default_sigpipe(command: &str) {
    if is_daemon_invocation(command) || matches!(command, "hook" | "inject" | "dismiss") {
        return;
    }
    use nix::sys::signal::{SigHandler, Signal, signal};
    // SAFETY: single-threaded at this point, and SIG_DFL installs no handler code.
    unsafe {
        let _ = signal(Signal::SIGPIPE, SigHandler::SigDfl);
    }
}

/// Tracing filter for the daemon when `RUST_LOG` is unset. A bare level (`debug`) applies to
/// Waymux and axum; anything with `=` or `,` is taken as a full `EnvFilter` directive.
fn log_filter(log_level: Option<&str>) -> String {
    match log_level.map(str::trim) {
        Some(directive) if directive.contains(['=', ',']) => directive.to_string(),
        Some(level) if !level.is_empty() => format!("waymux={level},acpd={level},axum={level}"),
        _ => "waymux=info,acpd=info,axum=debug".to_string(),
    }
}

pub fn entry() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let bin_name = args
        .first()
        .and_then(|s| Path::new(s).file_name())
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "waymux".to_string());

    if bin_name == "awt" {
        let code = wt::run_cli(&args[1..]);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    let first_arg = args.get(1).map(|s| s.as_str()).unwrap_or("");
    restore_default_sigpipe(first_arg);

    if let Some(spec) = find_command(first_arg)
        && !has_own_help(first_arg)
        && args.get(2).is_some_and(|a| is_help_flag(a))
    {
        print_command_help(&bin_name, spec);
        return Ok(());
    }

    // Fast-path synchronous subcommands (zero Tokio startup, <1.5ms execution)
    let exit_code = match first_arg {
        "--version" | "-V" => {
            println!("{} {}", bin_name, env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "--help" | "-h" | "help" => {
            print_help(&bin_name);
            return Ok(());
        }
        "health" => Some(rpc_client::check_health_cli()),
        "status" => Some(rpc_client::status_cli()),
        "list" => Some(rpc_client::list_agents_cli()),
        "dismiss" => Some(rpc_client::dismiss_pane_cli(&args[2..])),
        "cockpit" => Some(cockpit::run_cockpit(&args[2..])),
        "cockpit-items" => Some(cockpit::items::run_cli(&args[2..])),
        "cockpit-preview" => Some(cockpit::preview::run_cli(&args[2..])),
        "cockpit-diff" => Some(cockpit::diff::run_cli(&args[2..])),
        "cockpit-help" => Some(cockpit::help::run_cli()),
        "agent-action" => Some(cockpit::actions::run_agent_action(&args[2..])),
        "review-notes" => Some(cockpit::notes::run_cli(&args[2..])),
        "window-create" => Some(cockpit::actions::run_window_create()),
        "window-delete" => Some(cockpit::actions::run_window_delete()),
        "triage" => Some(triage::run_cli(&args[2..])),
        "hook" => Some(hook::run_cli(&args[2..])),
        "inject" => Some(inject::run_cli(&args[2..])),
        "wt" | "awt" => Some(wt::run_cli(&args[2..])),
        "icon" => Some(icons::run_icon_cli(&args[2..])),
        "icons" => Some(icons::run_icons_cli(&args[2..])),
        "init" => Some(init::run_cli(&args[2..])),
        "doctor" => Some(doctor::run_cli()),
        other if !is_daemon_invocation(other) => {
            eprintln!(
                "{bin_name}: unknown command '{other}'\nRun '{bin_name} --help' for the list of commands."
            );
            Some(2)
        }
        _ => None,
    };

    if let Some(code) = exit_code {
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    // Daemon Mode: initialize Tokio multi-threaded runtime and tracing
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let mut config_path = None;
        let mut iter = args.iter().skip(1);
        while let Some(arg) = iter.next() {
            if arg == "daemon" {
                continue;
            } else if arg == "--config" || arg == "-c" {
                config_path = iter.next().cloned();
            } else if let Some(stripped) = arg.strip_prefix("--config=") {
                config_path = Some(stripped.to_string());
            }
        }

        // The config is read before tracing starts so that its `log_level` can set the filter.
        let (config, loaded_from) = crate::config::Config::resolve(config_path.as_deref())?;

        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| log_filter(config.log_level.as_deref()).into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();

        tracing::info!("Starting Waymux Daemon...");
        match &loaded_from {
            Some(path) => tracing::info!("Loaded config from {}", path),
            None => tracing::info!("No external config file found, using built-in defaults"),
        }

        // Default to the per-user runtime dir (systemd's RuntimeDirectory=waymux) so a second
        // daemon is refused even without a config file.
        let pid_path = match config.pid_file.clone() {
            Some(path) => path,
            None => {
                let dir = crate::presets::private_runtime_dir().ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is not a directory owned by you; set pid_file in the config",
                        crate::presets::runtime_dir().display()
                    )
                })?;
                dir.join("waymux.pid").to_string_lossy().to_string()
            }
        };
        tracing::info!("Creating PID file at {}", pid_path);
        let _pid_file = crate::pid::PidFile::create(&pid_path)?;

        crate::daemon::run(config, config_path).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_filter_from_config_level() {
        assert_eq!(log_filter(None), "waymux=info,acpd=info,axum=debug");
        assert_eq!(log_filter(Some("  ")), "waymux=info,acpd=info,axum=debug");
        assert_eq!(
            log_filter(Some("debug")),
            "waymux=debug,acpd=debug,axum=debug"
        );
        assert_eq!(log_filter(Some("waymux=trace")), "waymux=trace");
    }

    #[test]
    fn test_daemon_invocation_only_for_bare_daemon_or_config() {
        for arg in ["", "daemon", "-c", "--config", "--config=/tmp/x.toml"] {
            assert!(is_daemon_invocation(arg), "{arg:?} should start the daemon");
        }
        for arg in ["stauts", "--verbose", "status --help", "Daemon"] {
            assert!(
                !is_daemon_invocation(arg),
                "{arg:?} must not start the daemon"
            );
        }
    }

    #[test]
    fn test_command_table_names_are_unique_and_documented() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate command names");
        for c in COMMANDS {
            assert!(
                c.usage.starts_with(c.name),
                "{} usage must start with its name",
                c.name
            );
            assert!(!c.about.is_empty());
        }
    }

    #[test]
    fn test_help_flag_detection() {
        assert!(is_help_flag("-h") && is_help_flag("--help"));
        assert!(!is_help_flag("help") && !is_help_flag("-H"));
        assert!(has_own_help("wt") && has_own_help("init"));
        assert!(!has_own_help("status"));
    }
}
