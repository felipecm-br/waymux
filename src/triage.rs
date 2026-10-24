use crate::cockpit::items::{contains_agent_name, is_agent_binary_name, pane_needs_review};
use crate::rpc_client::{self, AgentPaneEntry};
use crate::theme::ThemePalette;
use crate::tmux_cmd::{
    tmux_display_msg_timed, tmux_get_global_opt, tmux_out, tmux_set_global_opt, tmux_status,
    tmux_unset_global_opt,
};
use std::collections::{HashMap, HashSet};
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

/// Idle panes whose finished work nobody has reviewed (the Cockpit's `review` rows), oldest
/// completion first. tmux is asked for pane paths only when some idle pane has a completion.
fn review_panes(daemon: &HashMap<String, AgentPaneEntry>) -> Vec<String> {
    let mut done: Vec<(u64, &String)> = daemon
        .iter()
        .filter(|(_, v)| v.state == "idle" && v.finished_at > 0)
        .map(|(p, v)| (normalize_ts(v.finished_at), p))
        .collect();
    if done.is_empty() {
        return Vec::new();
    }
    let listing = tmux_out(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_current_path}"])
        .unwrap_or_default();
    let paths: HashMap<&str, &str> = listing.lines().filter_map(|l| l.split_once('\t')).collect();
    done.sort();
    done.into_iter()
        .filter(|(_, p)| {
            paths
                .get(p.as_str())
                .is_some_and(|path| pane_needs_review(path, daemon[*p].finished_at))
        })
        .map(|(_, p)| p.clone())
        .collect()
}

/// The panes triage visits, plus which of them are there for review rather than waiting.
pub struct TriageQueue {
    pub panes: Vec<String>,
    pub review: HashSet<String>,
}

pub fn collect_notifying_panes() -> TriageQueue {
    let daemon = rpc_client::fetch_agent_states();
    let reviews = review_panes(&daemon);
    let panes = select_notifying(
        &daemon,
        &reviews,
        || {
            tmux_out(&[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{@ai_agent_state_raw}",
            ])
            .unwrap_or_default()
        },
        pane_exists,
        || tmux_get_global_opt("@ai_agent_last_bell"),
        || {
            tmux_out(&[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{pane_current_command}\t#{pane_title}",
            ])
            .unwrap_or_default()
        },
    );
    TriageQueue {
        panes,
        review: reviews.into_iter().collect(),
    }
}

/// The panes triage visits, from the first source that yields any:
///
/// 1. agents the daemon reports waiting on you, in `order_urgent` order, then `reviews`
///    (finished work nobody has reviewed), the order the Cockpit sorts them in
/// 2. agents the daemon reports working, most recent first
/// 3. waiting states recorded on panes (`@ai_agent_state_raw`), for a daemon that is down
/// 4. the pane that rang the bell last
/// 5. panes recorded as working, then any pane with a recorded agent state
/// 6. panes running an agent binary or with an agent in their title
///
/// tmux is reached only through the closures, so the order is testable.
fn select_notifying(
    daemon: &HashMap<String, AgentPaneEntry>,
    reviews: &[String],
    tmux_states: impl FnOnce() -> String,
    exists: impl Fn(&str) -> bool,
    last_bell: impl FnOnce() -> String,
    pane_commands: impl FnOnce() -> String,
) -> Vec<String> {
    let urgent = order_urgent(
        daemon
            .iter()
            .map(|(k, v)| (k.clone(), v.state.clone(), v.last_timestamp)),
    );
    let notifying: Vec<String> = urgent
        .into_iter()
        .chain(reviews.iter().cloned())
        .filter(|p| exists(p))
        .collect();
    if !notifying.is_empty() {
        return notifying;
    }

    let mut active: Vec<(&String, u64)> = daemon
        .iter()
        .filter(|(_, v)| matches!(v.state.as_str(), "working" | "busy"))
        .map(|(k, v)| (k, v.last_timestamp))
        .collect();
    active.sort_by_key(|b| std::cmp::Reverse(b.1));
    let notifying: Vec<String> = active
        .into_iter()
        .filter(|(p, _)| exists(p))
        .map(|(p, _)| p.clone())
        .collect();
    if !notifying.is_empty() {
        return notifying;
    }

    let list_raw = tmux_states();
    let recorded: Vec<(&str, &str)> = list_raw
        .lines()
        .filter_map(|line| {
            let (p, st) = line.split_once('\t')?;
            Some((p, st.trim()))
        })
        .collect();

    let notifying = order_urgent(
        recorded
            .iter()
            .map(|(p, st)| (p.to_string(), st.to_string(), 0)),
    );
    if !notifying.is_empty() {
        return notifying;
    }

    let bell = last_bell();
    if exists(&bell) {
        return vec![bell];
    }

    let with_state = |states: &[&str]| -> Vec<String> {
        recorded
            .iter()
            .filter(|(_, st)| states.contains(st))
            .map(|(p, _)| p.to_string())
            .collect()
    };
    let working = with_state(&["working", "busy"]);
    if !working.is_empty() {
        return working;
    }
    let tagged = with_state(&[
        "idle",
        "working",
        "busy",
        "question",
        "permission",
        "error",
        "awaiting_input",
        "stalled",
    ]);
    if !tagged.is_empty() {
        return tagged;
    }

    pane_commands()
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            (parts.len() >= 3 && (is_agent_binary_name(parts[1]) || contains_agent_name(parts[2])))
                .then(|| parts[0].to_string())
        })
        .collect()
}

/// Sound played when triage lands on a pane in `state`.
fn alert_sound(state: &str) -> &'static str {
    match state {
        "question" | "awaiting_input" => "02-gentle-ping.wav",
        "permission" => "06-cyber-pulse.wav",
        "error" | "stalled" => "10-arcade-blip.wav",
        _ => "04-subtle-bell.wav",
    }
}

pub fn run_popup() -> i32 {
    let notifying = collect_notifying_panes().panes;
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
                &format!(" 󰮯 Already focused on active agent ({sess} › {win_name})"),
            );
            return 0;
        }
    }
    tmux_set_global_opt("@ai_agent_bell_last_pane", &pane);

    if current_sess == sess && current_win == win_idx {
        let _ = tmux_status(&["select-pane", "-t", &pane]);
        tmux_display_msg_timed(
            1500,
            &format!(" 󰮯 AI Agent: focused split pane ({sess} › {win_name})"),
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

    let src_win = format!("{sess}:{win_idx}");
    let dst_win1 = format!("{popup_sess}:1");
    let dst_win0 = format!("{popup_sess}:0");
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
        "tmux attach-session -t \"{popup_sess}:1\"; tmux unlink-window -t \"{popup_sess}:1\" >/dev/null 2>&1 || true"
    );

    let _ = tmux_status(&[
        "display-popup",
        "-S",
        &format!("fg={}", theme.yellow),
        "-s",
        &format!("fg={text_col}"),
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
    let queue = collect_notifying_panes();
    let notifying = queue.panes;
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
            let target_win = format!("{origin_sess}:{origin_win}");
            let _ = tmux_status(&["select-window", "-t", &target_win]);
            let _ = tmux_status(&["select-pane", "-t", &origin_pane]);

            tmux_display_msg_timed(
                1500,
                &format!(" 󰌑 AI Triage: Returned to {origin_sess}:{origin_win}"),
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
    let target_state = if queue.review.contains(target_pane) {
        "review │ Ctrl+G reviews".to_string()
    } else {
        target_state
    };

    if current_sess != target_sess {
        let _ = tmux_status(&["switch-client", "-t", &target_sess]);
    }
    let win_spec = format!("{target_sess}:{target_win}");
    let _ = tmux_status(&["select-window", "-t", &win_spec]);
    let _ = tmux_status(&["select-pane", "-t", target_pane]);

    if let Ok(home) = std::env::var("HOME") {
        let sound_rel = alert_sound(&target_state);
        let sound_file = format!("{home}/.local/share/sounds/ai/{sound_rel}");
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
        let queue: Vec<String> = ["%a", "%b", "%c"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(next_in_queue(&queue, "%x").unwrap(), "%a");
        assert_eq!(next_in_queue(&queue, "%a").unwrap(), "%b");
        assert_eq!(next_in_queue(&queue, "%c").unwrap(), "%a");

        // After %a is serviced it leaves the queue; from %a the next jump is %b, not %c.
        let serviced: Vec<String> = ["%b", "%c"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(next_in_queue(&serviced, "%a").unwrap(), "%b");
        assert!(next_in_queue(&[], "%a").is_none());
    }

    fn daemon(entries: &[(&str, &str, u64)]) -> HashMap<String, AgentPaneEntry> {
        entries
            .iter()
            .map(|(pane, state, ts)| {
                (
                    pane.to_string(),
                    AgentPaneEntry {
                        state: state.to_string(),
                        last_timestamp: *ts,
                        message: None,
                        finished_at: 0,
                    },
                )
            })
            .collect()
    }

    /// Runs the selection with the given tmux outputs; every pane but `%gone` exists.
    fn select(
        daemon: &HashMap<String, AgentPaneEntry>,
        states: &str,
        bell: &str,
        commands: &str,
    ) -> Vec<String> {
        select_notifying(
            daemon,
            &[],
            || states.to_string(),
            |p| p.starts_with('%') && p != "%gone",
            || bell.to_string(),
            || commands.to_string(),
        )
    }

    #[test]
    fn test_select_notifying_prefers_daemon_waiting_agents() {
        let d = daemon(&[
            ("%1", "working", 50),
            ("%2", "question", 20),
            ("%3", "permission", 30),
            ("%gone", "permission", 10),
        ]);
        let tmux_never = || -> String { panic!("tmux is not consulted when the daemon answers") };
        let picked = select_notifying(
            &d,
            &["%5".to_string()],
            tmux_never,
            |p| p != "%gone",
            tmux_never,
            tmux_never,
        );
        assert_eq!(
            picked,
            ["%3", "%2", "%5"],
            "class first, then work to review; panes that are gone are skipped"
        );
    }

    #[test]
    fn test_select_notifying_falls_back_in_order() {
        // Daemon: only working agents, newest first.
        let d = daemon(&[("%1", "working", 10), ("%2", "busy", 20)]);
        assert_eq!(select(&d, "", "", ""), ["%2", "%1"]);

        // Daemon down: waiting states recorded on panes.
        let none = HashMap::new();
        let states = "%4\tidle\n%5\tpermission\n%6\t question \nbroken-line\n";
        assert_eq!(select(&none, states, "%9", ""), ["%5", "%6"]);

        // Nothing waiting: the pane that rang last, if it still exists.
        assert_eq!(select(&none, "%4\tidle\n", "%9", ""), ["%9"]);
        // ...else working panes, else any pane with a state.
        assert_eq!(select(&none, "%4\tidle\n%7\tbusy\n", "%gone", ""), ["%7"]);
        assert_eq!(select(&none, "%4\tidle\n%8\tnormal\n", "", ""), ["%4"]);

        // Last resort: panes running an agent binary or titled after one.
        let commands = "%1\tzsh\tshell\n%2\tclaude\t-\n%3\tnode\tOpenCode session\n%4\tvim\n";
        assert_eq!(select(&none, "", "", commands), ["%2", "%3"]);
        assert_eq!(
            select(&none, "", "", "%1\tzsh\tshell\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_alert_sound_per_state() {
        assert_eq!(alert_sound("permission"), "06-cyber-pulse.wav");
        assert_eq!(alert_sound("question"), alert_sound("awaiting_input"));
        assert_eq!(alert_sound("stalled"), "10-arcade-blip.wav");
        assert_eq!(alert_sound("active"), "04-subtle-bell.wav");
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        fn arb_state() -> impl Strategy<Value = String> {
            prop::sample::select(vec![
                "permission",
                "question",
                "awaiting_input",
                "error",
                "stalled",
                "hanging",
                "working",
                "busy",
                "idle",
                "closed",
                "",
                "junk",
            ])
            .prop_map(str::to_string)
        }

        proptest! {
            /// The queue holds exactly the urgent panes, in class order, each once.
            #[test]
            fn order_urgent_is_a_sorted_selection(
                states in prop::collection::vec((arb_state(), any::<u64>()), 0..20),
            ) {
                // Pane ids are unique: the daemon keys its map by them.
                let entries: Vec<(String, String, u64)> = states
                    .into_iter()
                    .enumerate()
                    .map(|(i, (st, ts))| (format!("%{i}"), st, ts))
                    .collect();
                let class_of: HashMap<&str, u8> = entries
                    .iter()
                    .filter_map(|(p, st, _)| Some((p.as_str(), service_class(st)?)))
                    .collect();
                let queue = order_urgent(entries.clone());
                prop_assert_eq!(queue.len(), class_of.len());
                let classes: Vec<u8> = queue.iter().map(|p| class_of[p.as_str()]).collect();
                prop_assert!(classes.windows(2).all(|w| w[0] <= w[1]));
            }

            /// Jumping visits every queued pane once per cycle, wherever it starts.
            #[test]
            fn next_in_queue_cycles_through_everyone(len in 1usize..12, start in "%[0-9]{1,2}") {
                let queue: Vec<String> = (0..len).map(|i| format!("%{i}")).collect();
                let mut seen = std::collections::HashSet::new();
                let mut cur = start;
                for _ in 0..len {
                    let next = next_in_queue(&queue, &cur).unwrap().clone();
                    seen.insert(next.clone());
                    cur = next;
                }
                prop_assert_eq!(seen.len(), len);
            }
        }
    }
}
