use crate::cockpit::notes;
use crate::theme::{BOLD, RESET, ThemePalette, strip_ansi};
use crate::tmux_cmd::{tmux_out, tmux_out_raw};
use std::fmt::Write as _;

pub fn box_line(card_border: &str, w: usize, content: &str) -> String {
    let clean = strip_ansi(content);
    let len = clean.chars().count();
    let max_content = w.saturating_sub(4);
    let pad = max_content.saturating_sub(len);
    format!(
        "{}│{} {}{} {}│{}\n",
        card_border,
        RESET,
        content,
        " ".repeat(pad),
        card_border,
        RESET
    )
}

/// `risky: <rules> · y twice to allow` when the pane waits on a permission prompt for a
/// command the risk rules match. Red is reserved for this kind of operational urgency.
fn risk_banner(pane_id: &str) -> Option<String> {
    let states = crate::rpc_client::fetch_agent_states();
    let entry = states.get(pane_id).filter(|e| e.state == "permission")?;
    let cmd = super::actions::pending_command(pane_id, entry.message.as_deref())?;
    let rules = crate::risk::risky_rules(&cmd);
    if rules.is_empty() {
        return None;
    }
    Some(format!("󰳌 risky: {} · y twice to allow", rules.join(", ")))
}

/// What the agent did since you last focused its pane (once you have been away a while) and
/// what its last turn did.
fn recap(pane_id: &str) -> Vec<String> {
    let Ok(res) = crate::rpc_client::rpc_call(
        "agent/recap",
        &serde_json::json!({ "pane_id": pane_id }),
        150,
    ) else {
        return Vec::new();
    };
    let Ok(activity) = serde_json::from_value::<crate::activity::Activity>(res) else {
        return Vec::new();
    };
    let now = crate::sys::unix_ms();
    [
        crate::activity::recap_line(&activity, now),
        crate::activity::last_turn_line(&activity, now),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The prompts waiting in the pane's delivery queue, one short line each.
fn queued(pane_id: &str) -> Vec<String> {
    let Ok(res) = crate::rpc_client::rpc_call(
        "queue/list",
        &serde_json::json!({ "pane_id": pane_id }),
        150,
    ) else {
        return Vec::new();
    };
    queue_lines(&res, pane_id)
}

fn queue_lines(res: &serde_json::Value, pane_id: &str) -> Vec<String> {
    let items = res[pane_id].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("󰇮 {} queued (Prompt Stash · Tab):", items.len())];
    for item in items.iter().take(3) {
        let text = item["text"].as_str().unwrap_or("");
        let first: String = text.lines().next().unwrap_or("").chars().take(60).collect();
        let status = match item["status"].as_str().unwrap_or("") {
            "held" => " · held: draft in the prompt",
            "stalled" => " · stalled",
            "typed" => " · typed",
            _ => "",
        };
        out.push(format!("  {} {first}{status}", item["id"]));
    }
    if items.len() > 3 {
        out.push(format!("  +{} more", items.len() - 3));
    }
    out
}

pub fn run_cli(args: &[String]) -> i32 {
    let session = args.first().map_or("", std::string::String::as_str);
    let idx = args.get(1).map_or("", std::string::String::as_str);
    let mut pane_id = args
        .get(2)
        .map_or("", std::string::String::as_str)
        .to_string();
    let mut ppath = args
        .get(3)
        .map_or("", std::string::String::as_str)
        .to_string();

    if session.is_empty() || idx.is_empty() || idx == "-" {
        println!("  \x1b[38;2;146;131;116m(no active window selected)\x1b[0m");
        return 0;
    }

    let target_win = format!("{session}:{idx}");
    if pane_id.is_empty() || pane_id == "-" {
        pane_id = tmux_out(&["display-message", "-p", "-t", &target_win, "#{pane_id}"])
            .unwrap_or_default();
    }
    if ppath.is_empty() || ppath == "-" {
        ppath = tmux_out(&[
            "display-message",
            "-p",
            "-t",
            &target_win,
            "#{pane_current_path}",
        ])
        .unwrap_or_default();
    }

    let theme = ThemePalette::load();
    let c_gray = &theme.c_muted;

    let raw_terminal = tmux_out_raw(&["capture-pane", "-ep", "-S", "-120", "-t", &pane_id])
        .unwrap_or_default()
        .replace("\x1b[4m", "")
        .replace("\x1b[24m", "")
        .replace("\x1b[21m", "");

    let mut out = String::with_capacity(4096);
    if let Some(banner) = risk_banner(&pane_id) {
        let _ = writeln!(out, "{}{BOLD}{banner}{RESET}\n", theme.c_error);
    }
    let header: Vec<String> = recap(&pane_id)
        .into_iter()
        .chain(queued(&pane_id))
        .collect();
    if !header.is_empty() {
        for line in &header {
            let _ = writeln!(out, "{c_gray}{line}{RESET}");
        }
        out.push('\n');
    }
    let lines: Vec<&str> = raw_terminal.lines().collect();
    if let Some(last_non_empty) = lines.iter().rposition(|l| !l.trim().is_empty()) {
        for line in &lines[..=last_non_empty] {
            out.push_str(line);
            out.push('\n');
        }
    } else {
        let _ = writeln!(out, "  {c_gray}(pane output empty or detached){RESET}");
    }

    let notes_section = notes::render_pending_notes(&ppath);
    if !notes_section.is_empty() {
        out.push('\n');
        out.push_str(&notes_section);
    }

    print!("{out}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_lines_show_count_status_and_cap() {
        let res = serde_json::json!({ "%1": [
            { "id": 4, "text": "first line\nsecond", "status": "held" },
            { "id": 5, "text": "b", "status": "queued" },
            { "id": 6, "text": "c", "status": "stalled" },
            { "id": 7, "text": "d", "status": "queued" },
        ]});
        let lines = queue_lines(&res, "%1");
        assert_eq!(lines[0], "󰇮 4 queued (Prompt Stash · Tab):");
        assert_eq!(lines[1], "  4 first line · held: draft in the prompt");
        assert_eq!(lines[3], "  6 c · stalled");
        assert_eq!(lines.last().unwrap(), "  +1 more");
        assert!(queue_lines(&res, "%2").is_empty());
    }
}
