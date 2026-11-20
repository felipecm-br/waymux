//! Queued prompts survive a daemon restart (P6b).
//!
//! The prompts not typed yet (`queued` and `held`) are saved in `$XDG_STATE_HOME/waymux/queue`
//! (0600) after every change to the queue, and a starting daemon puts back the ones whose pane
//! still lives in the same tmux server. Typed and stalled prompts are never saved: the agent may
//! have taken them, and a prompt is typed at most once. As with the hook tokens, the file names
//! the tmux server it was written for, since a new server numbers its panes from `%0` again.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

use super::queue::{ItemStatus, MAX_BYTES, MAX_ITEMS, Queues};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedItem {
    pub id: u64,
    pub text: String,
    pub by: String,
    pub queued_at: u64,
}

pub type SavedPanes = BTreeMap<String, Vec<SavedItem>>;

#[derive(Serialize, Deserialize)]
struct Saved {
    /// `#{pid}` of the tmux server the panes belong to.
    tmux_pid: String,
    panes: SavedPanes,
}

/// The prompts worth keeping: those not typed yet, in queue order.
pub fn snapshot(queues: &Queues) -> SavedPanes {
    queues
        .iter()
        .filter_map(|(pane, q)| {
            let items: Vec<SavedItem> = q
                .items
                .iter()
                .filter(|i| matches!(i.status, ItemStatus::Queued | ItemStatus::Held))
                .map(|i| SavedItem {
                    id: i.id,
                    text: i.text.clone(),
                    by: i.by.clone(),
                    queued_at: i.queued_at,
                })
                .collect();
            (!items.is_empty()).then(|| (pane.clone(), items))
        })
        .collect()
}

pub fn encode(tmux_pid: &str, panes: &SavedPanes) -> String {
    serde_json::to_string(&Saved {
        tmux_pid: tmux_pid.to_string(),
        panes: panes.clone(),
    })
    .unwrap_or_default()
}

/// The saved prompts of `json` when it was written for tmux server `pid`, kept only for panes
/// in `live`, with the same limits `queue/push` enforces.
pub fn decode_for(json: &str, pid: &str, live: &HashSet<String>) -> SavedPanes {
    let Ok(saved) = serde_json::from_str::<Saved>(json) else {
        return SavedPanes::new();
    };
    if saved.tmux_pid != pid {
        return SavedPanes::new();
    }
    saved
        .panes
        .into_iter()
        .filter(|(pane, _)| super::hook_tokens::valid_pane(pane) && live.contains(pane))
        .filter_map(|(pane, items)| {
            let items: Vec<SavedItem> = items
                .into_iter()
                .filter(|i| !i.text.trim().is_empty() && i.text.len() <= MAX_BYTES)
                .take(MAX_ITEMS)
                .collect();
            (!items.is_empty()).then_some((pane, items))
        })
        .collect()
}

pub fn tmux_pid() -> Option<String> {
    crate::tmux_cmd::tmux_out(&["display-message", "-p", "#{pid}"]).filter(|p| !p.is_empty())
}

pub fn live_panes() -> HashSet<String> {
    crate::tmux_cmd::tmux_out(&["list-panes", "-a", "-F", "#{pane_id}"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::queue::{PaneQueue, QueueItem};

    fn item(id: u64, text: &str, status: ItemStatus) -> QueueItem {
        QueueItem {
            id,
            text: text.into(),
            by: "t".into(),
            status,
            queued_at: 7,
            force: false,
        }
    }

    #[test]
    fn test_snapshot_keeps_only_prompts_not_typed() {
        let mut queues = Queues::new();
        let mut q = PaneQueue::default();
        q.items.push_back(item(1, "typed", ItemStatus::Typed));
        q.items.push_back(item(2, "next", ItemStatus::Queued));
        q.items.push_back(item(3, "draft wait", ItemStatus::Held));
        queues.insert("%1".into(), q);
        let mut stalled = PaneQueue::default();
        stalled
            .items
            .push_back(item(4, "stuck", ItemStatus::Stalled));
        queues.insert("%2".into(), stalled);
        let snap = snapshot(&queues);
        assert_eq!(snap.len(), 1, "a pane with nothing waiting is not saved");
        let ids: Vec<u64> = snap["%1"].iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn test_decode_only_for_the_same_server_and_live_panes() {
        let mut panes = SavedPanes::new();
        let one = |id, t: &str| SavedItem {
            id,
            text: t.into(),
            by: "t".into(),
            queued_at: 1,
        };
        panes.insert("%1".into(), vec![one(1, "hello"), one(2, "  ")]);
        panes.insert("%2".into(), vec![one(3, "gone pane")]);
        panes.insert("bad".into(), vec![one(4, "not a pane")]);
        panes.insert("%3".into(), (0..20).map(|i| one(i, "many")).collect());
        let json = encode("4242", &panes);
        let live: HashSet<String> = ["%1", "%3", "bad"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let got = decode_for(&json, "4242", &live);
        assert_eq!(
            got["%1"],
            vec![one(1, "hello")],
            "blank prompts are dropped"
        );
        assert!(!got.contains_key("%2"), "a closed pane gets nothing back");
        assert!(!got.contains_key("bad"));
        assert_eq!(got["%3"].len(), MAX_ITEMS);

        assert!(
            decode_for(&json, "4243", &live).is_empty(),
            "another tmux server"
        );
        assert!(decode_for("not json", "4242", &live).is_empty());
        let big = encode(
            "1",
            &SavedPanes::from([("%1".into(), vec![one(1, &"x".repeat(MAX_BYTES + 1))])]),
        );
        assert!(decode_for(&big, "1", &live).is_empty());
    }
}
