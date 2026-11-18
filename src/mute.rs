//! `waymux mute`: silence one tmux window's alerts while its state still shows.
//!
//! A muted window (window option `@waymux_muted` = `1`) plays no sound and rings no status-line
//! bell or desktop notification, but its agents keep their state glyphs, urgency and Cockpit
//! rows, so the unread mark stays (idea from Orca 1.4.262: mute a workspace, keep its dot). The
//! phone alert for a prompt held while you are away is not muted: it is the one alert that
//! stands in for the desk.

use crate::tmux_cmd::{tmux_display_msg_timed, tmux_out, tmux_status};

pub const OPTION: &str = "@waymux_muted";

const USAGE: &str = "\
usage: waymux mute [<pane>] [--on | --off | --status]

  Toggles the alerts of the window holding <pane> (default: $TMUX_PANE): no sound, no status-line
  bell, no desktop notification. State glyphs, urgency and the Cockpit row stay.
  --on / --off   Set instead of toggling
  --status       Print `muted` or `unmuted` and exit 0 or 1";

/// Whether the window option value reads as muted.
pub fn is_muted_value(v: &str) -> bool {
    matches!(v.trim(), "1" | "on" | "true")
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Toggle,
    On,
    Off,
    Status,
}

fn parse(args: &[String]) -> Result<(Option<String>, Mode), String> {
    let mut pane = None;
    let mut mode = Mode::Toggle;
    for a in args {
        match a.as_str() {
            "--on" => mode = Mode::On,
            "--off" => mode = Mode::Off,
            "--status" => mode = Mode::Status,
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s if pane.is_none() => pane = Some(s.to_string()),
            s => return Err(format!("unexpected argument '{s}'")),
        }
    }
    Ok((pane, mode))
}

pub fn run_cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let (pane, mode) = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("waymux mute: {e} (see --help)");
            return 2;
        }
    };
    let Some(pane) = pane
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|p| !p.is_empty())
    else {
        eprintln!("waymux mute: no pane given and not inside tmux");
        return 2;
    };
    // A pane tmux cannot find prints an empty line on some versions instead of failing.
    let Some(info) = tmux_out(&[
        "display-message",
        "-p",
        "-t",
        &pane,
        &format!("#{{window_id}}\t#{{{OPTION}}}\t#I:#W"),
    ])
    .filter(|i| i.starts_with('@')) else {
        eprintln!("waymux mute: no such pane: {pane}");
        return crate::verbs::exit::NOT_FOUND;
    };
    let mut f = info.splitn(3, '\t');
    let (window, current, label) = (
        f.next().unwrap_or_default(),
        is_muted_value(f.next().unwrap_or_default()),
        f.next().unwrap_or_default(),
    );
    let want = match mode {
        Mode::Status => {
            println!("{}", if current { "muted" } else { "unmuted" });
            return i32::from(!current);
        }
        Mode::On => true,
        Mode::Off => false,
        Mode::Toggle => !current,
    };
    let ok = if want {
        tmux_status(&["set-option", "-w", "-t", window, OPTION, "1"])
    } else {
        tmux_status(&["set-option", "-w", "-u", "-t", window, OPTION])
    };
    if !ok {
        eprintln!("waymux mute: tmux refused to set {OPTION} on {window}");
        return 1;
    }
    let msg = if want {
        format!("󰝟 {label} muted: no sounds or alerts, state still shows")
    } else {
        format!("󰕾 {label} alerts on again")
    };
    tmux_display_msg_timed(3000, &msg);
    0
}

/// Whether the window holding `pane_id` is muted (async, for the daemon's adapters).
pub async fn pane_muted(pane_id: &str) -> bool {
    let out = crate::tmux_cmd::async_command()
        .args([
            "display-message",
            "-p",
            "-t",
            pane_id,
            &format!("#{{{OPTION}}}"),
        ])
        .output()
        .await;
    out.is_ok_and(|o| is_muted_value(&String::from_utf8_lossy(&o.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn arguments_parse_into_a_pane_and_a_mode() {
        assert_eq!(parse(&s(&[])), Ok((None, Mode::Toggle)));
        assert_eq!(
            parse(&s(&["%3", "--on"])),
            Ok((Some("%3".into()), Mode::On))
        );
        assert_eq!(parse(&s(&["--off"])), Ok((None, Mode::Off)));
        assert_eq!(
            parse(&s(&["--status", "%1"])),
            Ok((Some("%1".into()), Mode::Status))
        );
        assert!(parse(&s(&["--loud"])).is_err());
        assert!(parse(&s(&["%1", "%2"])).is_err());
    }

    #[test]
    fn only_set_values_read_as_muted() {
        for v in ["1", "on", "true", " 1\n"] {
            assert!(is_muted_value(v), "{v:?}");
        }
        for v in ["", "0", "off", "no"] {
            assert!(!is_muted_value(v), "{v:?}");
        }
    }
}
