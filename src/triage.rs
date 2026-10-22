use crate::cockpit::items::{contains_agent_name, is_agent_binary_name};
use crate::rpc_client;
use crate::theme::ThemePalette;
use crate::tmux_cmd::{
    tmux_display_msg_timed, tmux_get_global_opt, tmux_out, tmux_set_global_opt, tmux_status,
    tmux_unset_global_opt,
};
use std::path::Path;
use std::process::{Command, Stdio};

fn pane_exists(pane_id: &str) -> bool {
    !pane_id.is_empty()
        && pane_id.starts_with('%')
        && tmux_out(&["display-message", "-t", pane_id, "-p", "#{pane_id}"]).is_some()
}

/// Service order for agents blocked on the human; lower is served first. Quick y/n
/// decisions go before replies and replies before diagnosis (shortest job first), which
/// lowers the mean time agents sit blocked. The Cockpit sorts with the same classes.
pub fn service_class(state: &str) -> Option<u8> {
    match state {
        "permission" => Some(0),
        "question" | "awaiting_input" => Some(1),
        "error" | "stalled" | "hanging" => Some(2),
        _ => None,
    }
}

/// Hooks report milliseconds, rehydrated panes seconds; unknown (0) sorts last.
pub fn normalize_ts(ts: u64) -> u64 {
    match ts {
        0 => u64::MAX,
        t if t < 10_000_000_000 => t * 1000,
        t => t,
    }
}

/// Urgent panes by service class, then longest-waiting first (FIFO bounds the wait of the
/// oldest request), then pane id for a stable order. Non-urgent entries are dropped.
pub fn order_urgent(entries: impl IntoIterator<Item = (String, String, u64)>) -> Vec<String> {
    let mut queue: Vec<(u8, u64, String)> = entries
        .into_iter()
        .filter_map(|(pane, state, ts)| service_class(&state).map(|c| (c, normalize_ts(ts), pane)))
        .collect();
    queue.sort();
    queue.into_iter().map(|(_, _, pane)| pane).collect()
}

/// The entry after `current` in queue order, else the head. Stateless, so servicing an
/// agent (which drops it from the queue) never makes the next jump skip someone.
pub fn next_in_queue<'a>(queue: &'a [String], current: &str) -> Option<&'a String> {
    match queue.iter().position(|p| p == current) {
        Some(i) => queue.get((i + 1) % queue.len()),
        None => queue.first(),
    }
}

pub fn collect_notifying_panes() -> Vec<String> {
    let mut notifying = Vec::new();
    let acpd_map = rpc_client::fetch_agent_states();

    if !acpd_map.is_empty() {
        let urgent = order_urgent(
            acpd_map
                .iter()
                .map(|(k, v)| (k.clone(), v.state.clone(), v.last_timestamp)),
        );
        notifying.extend(urgent.into_iter().filter(|p| pane_exists(p)));

        if notifying.is_empty() {
            let mut active: Vec<(&String, u64)> = acpd_map
                .iter()
                .filter(|(_, v)| matches!(v.state.as_str(), "working" | "busy"))
                .map(|(k, v)| (k, v.last_timestamp))
                .collect();
            active.sort_by_key(|b| std::cmp::Reverse(b.1));

            for (p, _) in active {
                if pane_exists(p) {
                    notifying.push(p.clone());
                }
            }
        }
    }

    let list_raw = tmux_out(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{@ai_agent_state_raw}",
    ])
    .unwrap_or_default();

    if notifying.is_empty() {
        notifying = order_urgent(list_raw.lines().filter_map(|line| {
            let (p, st) = line.split_once('\t')?;
            Some((p.to_string(), st.trim().to_string(), 0))
        }));
    }

    if notifying.is_empty() {
        let last_bell = tmux_get_global_opt("@ai_agent_last_bell");
        if pane_exists(&last_bell) {
            notifying.push(last_bell);
        }
    }

    if notifying.is_empty() {
        for line in list_raw.lines() {
            if let Some((p, st)) = line.split_once('\t')
                && matches!(st.trim(), "working" | "busy")
            {
                notifying.push(p.to_string());
            }
        }
    }

    if notifying.is_empty() {
        for line in list_raw.lines() {
            if let Some((p, st)) = line.split_once('\t')
                && matches!(
                    st.trim(),
                    "idle"
                        | "working"
                        | "busy"
                        | "question"
                        | "permission"
                        | "error"
                        | "awaiting_input"
                        | "stalled"
                )
            {
                notifying.push(p.to_string());
            }
        }
    }

    if notifying.is_empty() {
        let cmd_raw = tmux_out(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_current_command}\t#{pane_title}",
        ])
        .unwrap_or_default();
        for line in cmd_raw.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let p = parts[0];
                let cmd = parts[1];
                let title = parts[2];
                if is_agent_binary_name(cmd) || contains_agent_name(title) {
                    notifying.push(p.to_string());
                }
            }
        }
    }

    notifying
}

pub fn run_popup() -> i32 {
    let notifying = collect_notifying_panes();
    if notifying.is_empty() {
        tmux_display_msg_timed(1500, " 󰮯 No active AI Agent notification.");
        return 0;
    }

    let total = notifying.len();
    // Peek at the entry after the one shown last time; if that agent was serviced (and left
    // the queue) this falls back to the head, so nobody is skipped.
    let last_peeked = tmux_get_global_opt("@ai_agent_bell_last_pane");
    let mut pane = next_in_queue(&notifying, &last_peeked)
        .cloned()
        .unwrap_or_default();

    let current_pane = tmux_out(&["display-message", "-p", "#{pane_id}"]).unwrap_or_default();
    let current_sess = tmux_out(&["display-message", "-p", "#S"]).unwrap_or_default();
    let current_win = tmux_out(&["display-message", "-p", "#I"]).unwrap_or_default();

    let mut sess = tmux_out(&["display-message", "-t", &pane, "-p", "#S"]).unwrap_or_default();
    let mut win_idx = tmux_out(&["display-message", "-t", &pane, "-p", "#I"]).unwrap_or_default();
    let mut win_name = tmux_out(&["display-message", "-t", &pane, "-p", "#W"]).unwrap_or_default();

    if current_pane == pane {
        if total > 1 {
            pane = next_in_queue(&notifying, &pane)
                .cloned()
                .unwrap_or_default();
            sess = tmux_out(&["display-message", "-t", &pane, "-p", "#S"]).unwrap_or_default();
            win_idx = tmux_out(&["display-message", "-t", &pane, "-p", "#I"]).unwrap_or_default();
            win_name = tmux_out(&["display-message", "-t", &pane, "-p", "#W"]).unwrap_or_default();
        } else {
            tmux_display_msg_timed(
                1500,
                &format!(
                    " 󰮯 Already focused on active agent ({} › {})",
                    sess, win_name
                ),
            );
            return 0;
        }
    }
    tmux_set_global_opt("@ai_agent_bell_last_pane", &pane);

    if current_sess == sess && current_win == win_idx {
        let _ = tmux_status(&["select-pane", "-t", &pane]);
        tmux_display_msg_timed(
            1500,
            &format!(" 󰮯 AI Agent: focused split pane ({} › {})", sess, win_name),
        );
        return 0;
    }

    let popup_sess = "_popups";
    if !tmux_status(&["has-session", "-t", popup_sess]) {
        let _ = tmux_status(&["new-session", "-d", "-s", popup_sess, "-n", "_dummy"]);
    }

    if let Some(wins) = tmux_out(&[
        "list-windows",
        "-t",
        popup_sess,
        "-F",
        "#{window_id}:#{window_index}",
    ]) {
        for w in wins.lines() {
            if let Some((w_id, w_i)) = w.split_once(':')
                && w_i != "0"
                && !tmux_status(&["unlink-window", "-t", w_id])
            {
                let _ = tmux_status(&["kill-window", "-t", w_id]);
            }
        }
    }

    let src_win = format!("{}:{}", sess, win_idx);
    let dst_win1 = format!("{}:1", popup_sess);
    let dst_win0 = format!("{}:0", popup_sess);
    if !tmux_status(&["link-window", "-s", &src_win, "-t", &dst_win1]) {
        let _ = tmux_status(&["link-window", "-a", "-s", &src_win, "-t", &dst_win0]);
    }
    let _ = tmux_status(&["select-window", "-t", &dst_win1]);
    let _ = tmux_status(&["select-pane", "-t", &pane]);
    let _ = tmux_status(&["set-option", "-t", popup_sess, "status", "off"]);
    let _ = tmux_status(&["set-option", "-t", popup_sess, "key-table", "popup"]);

    let theme = ThemePalette::load();
    let text_col = std::env::var("TMUX_POPUP_TEXT_COLOR").unwrap_or_else(|_| "default".into());
    let attach_cmd = format!(
        "tmux attach-session -t \"{}:1\"; tmux unlink-window -t \"{}:1\" >/dev/null 2>&1 || true",
        popup_sess, popup_sess
    );

    let _ = tmux_status(&[
        "display-popup",
        "-S",
        &format!("fg={}", theme.yellow),
        "-s",
        &format!("fg={}", text_col),
        "-T",
        " 󰮯 ",
        "-w",
        "80%",
        "-h",
        "75%",
        "-b",
        "rounded",
        "-E",
        &attach_cmd,
    ]);

    0
}

pub fn run_jump() -> i32 {
    let notifying = collect_notifying_panes();
    let current_pane = tmux_out(&["display-message", "-p", "#{pane_id}"]).unwrap_or_default();
    let current_sess = tmux_out(&["display-message", "-p", "#S"]).unwrap_or_default();
    let current_win = tmux_out(&["display-message", "-p", "#I"]).unwrap_or_default();

    let origin_pane = tmux_get_global_opt("@ai_agent_triage_origin_pane");
    let origin_sess = tmux_get_global_opt("@ai_agent_triage_origin_sess");
    let origin_win = tmux_get_global_opt("@ai_agent_triage_origin_win");

    if notifying.is_empty() {
        if pane_exists(&origin_pane) {
            tmux_unset_global_opt("@ai_agent_triage_origin_pane");
            tmux_unset_global_opt("@ai_agent_triage_origin_sess");
            tmux_unset_global_opt("@ai_agent_triage_origin_win");

            if current_sess != origin_sess {
                let _ = tmux_status(&["switch-client", "-t", &origin_sess]);
            }
            let target_win = format!("{}:{}", origin_sess, origin_win);
            let _ = tmux_status(&["select-window", "-t", &target_win]);
            let _ = tmux_status(&["select-pane", "-t", &origin_pane]);

            tmux_display_msg_timed(
                1500,
                &format!(" 󰌑 AI Triage: Returned to {}:{}", origin_sess, origin_win),
            );
            return 0;
        }
        tmux_display_msg_timed(1500, " 󰮯 No active AI Agent alerts or return origin.");
        return 0;
    }

    let current_is_agent = tmux_out(&[
        "show-option",
        "-p",
        "-t",
        &current_pane,
        "-v",
        "@ai_agent_state_raw",
    ])
    .unwrap_or_default();

    if origin_pane.is_empty() && current_is_agent.is_empty() {
        tmux_set_global_opt("@ai_agent_triage_origin_pane", &current_pane);
        tmux_set_global_opt("@ai_agent_triage_origin_sess", &current_sess);
        tmux_set_global_opt("@ai_agent_triage_origin_win", &current_win);
    }

    let total = notifying.len();
    let Some(target_pane) = next_in_queue(&notifying, &current_pane) else {
        return 0;
    };
    let curr_idx = notifying.iter().position(|p| p == target_pane).unwrap_or(0);

    let target_sess =
        tmux_out(&["display-message", "-t", target_pane, "-p", "#S"]).unwrap_or_default();
    let target_win =
        tmux_out(&["display-message", "-t", target_pane, "-p", "#I"]).unwrap_or_default();
    let target_win_name =
        tmux_out(&["display-message", "-t", target_pane, "-p", "#W"]).unwrap_or_default();
    let target_state = tmux_out(&[
        "show-option",
        "-p",
        "-t",
        target_pane,
        "-v",
        "@ai_agent_state_raw",
    ])
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "active".to_string());

    if current_sess != target_sess {
        let _ = tmux_status(&["switch-client", "-t", &target_sess]);
    }
    let win_spec = format!("{}:{}", target_sess, target_win);
    let _ = tmux_status(&["select-window", "-t", &win_spec]);
    let _ = tmux_status(&["select-pane", "-t", target_pane]);

    if let Ok(home) = std::env::var("HOME") {
        let sound_rel = match target_state.as_str() {
            "question" | "awaiting_input" => "02-gentle-ping.wav",
            "permission" => "06-cyber-pulse.wav",
            "error" | "stalled" => "10-arcade-blip.wav",
            _ => "04-subtle-bell.wav",
        };
        let sound_file = format!("{}/.local/share/sounds/ai/{}", home, sound_rel);
        if Path::new(&sound_file).is_file() {
            let _ = Command::new(crate::sys::default_sound_player())
                .arg(&sound_file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    tmux_display_msg_timed(
        2000,
        &format!(
            " 󰮯 AI Triage: {} › {} ({}/{}) [{}] │ Prefix+I to return",
            target_sess,
            target_win_name,
            curr_idx + 1,
            total,
            target_state
        ),
    );

    0
}

pub fn run_cli(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|a| a == "--jump" || a == "jump" || a == "-j")
    {
        run_jump()
    } else {
        run_popup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pane: &str, state: &str, ts: u64) -> (String, String, u64) {
        (pane.to_string(), state.to_string(), ts)
    }

    #[test]
    fn test_service_class_order() {
        assert!(service_class("permission") < service_class("question"));
        assert_eq!(service_class("question"), service_class("awaiting_input"));
        assert!(service_class("question") < service_class("error"));
        assert_eq!(service_class("error"), service_class("stalled"));
        assert_eq!(service_class("busy"), None);
        assert_eq!(service_class("idle"), None);
    }

    #[test]
    fn test_normalize_ts_handles_seconds_millis_and_unknown() {
        assert_eq!(normalize_ts(1_700_000_000), 1_700_000_000_000);
        assert_eq!(normalize_ts(1_700_000_000_000), 1_700_000_000_000);
        assert_eq!(normalize_ts(0), u64::MAX);
    }

    #[test]
    fn test_order_urgent_is_class_then_oldest_first() {
        let queue = order_urgent([
            entry("%1", "question", 3_000),
            entry("%2", "permission", 9_000),
            entry("%3", "busy", 1_000),
            entry("%4", "question", 2_000),
            entry("%5", "stalled", 1_000),
            entry("%6", "permission", 0),
        ]);
        // permission (oldest, then unknown ts), question (oldest first), stalled; busy dropped.
        assert_eq!(queue, vec!["%2", "%6", "%4", "%1", "%5"]);
    }

    #[test]
    fn test_order_urgent_mixes_second_and_millisecond_timestamps() {
        // 1_700_000_000 s is older than 1_700_000_500_000 ms.
        let queue = order_urgent([
            entry("%new", "question", 1_700_000_500_000),
            entry("%old", "question", 1_700_000_000),
        ]);
        assert_eq!(queue, vec!["%old", "%new"]);
    }

    #[test]
    fn test_next_in_queue_never_skips_after_service() {
        let queue: Vec<String> = ["%a", "%b", "%c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_in_queue(&queue, "%x").unwrap(), "%a");
        assert_eq!(next_in_queue(&queue, "%a").unwrap(), "%b");
        assert_eq!(next_in_queue(&queue, "%c").unwrap(), "%a");

        // After %a is serviced it leaves the queue; from %a the next jump is %b, not %c.
        let serviced: Vec<String> = ["%b", "%c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_in_queue(&serviced, "%a").unwrap(), "%b");
        assert!(next_in_queue(&[], "%a").is_none());
    }
}
