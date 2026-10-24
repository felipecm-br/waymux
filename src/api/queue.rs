//! Delivery queue: prompts for an agent pane are held in the daemon and typed only once the
//! agent has been at rest for a second, one per rest, never into a permission prompt or the
//! middle of a turn.
//!
//! A typed prompt stays at the head until the pane shows `working` (the agent took it). One
//! the agent does not take within the stall window is marked `stalled` and never typed again,
//! so text is never typed twice; it holds the ones behind it until the pane next works or the
//! user drops it with `waymux queue rm`.

use super::state::ApiState;
use crate::adapters::AgentState;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

/// How long a pane must stay `idle`, with no state change, before a prompt is typed: an agent
/// flickers to idle for an instant between two tool calls.
pub const REST_MS: u64 = 1000;
/// How long the agent has to show `working` after a prompt is typed.
pub const STALL_MS: u64 = 5000;
pub const MAX_ITEMS: usize = 8;
pub const MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Queued,
    Typed,
    Stalled,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub id: u64,
    pub text: String,
    pub by: String,
    pub status: ItemStatus,
    pub queued_at: u64,
}

#[derive(Debug, Default)]
pub struct PaneQueue {
    pub items: VecDeque<QueueItem>,
    /// The pane state seq a delivery is already scheduled for (0 for a pane with no agent
    /// state; seqs start at 1), so a burst of updates schedules one delivery.
    scheduled: Option<u64>,
}

pub type Queues = HashMap<String, PaneQueue>;

#[derive(Debug, PartialEq, Eq)]
pub enum PushError {
    Empty,
    TooLarge,
    Full,
}

impl PushError {
    pub fn code(&self) -> &'static str {
        match self {
            PushError::Empty => "empty",
            PushError::TooLarge => "too_large",
            PushError::Full => "queue_full",
        }
    }
}

/// A pane may be typed into when its agent is idle, or when the daemon has no agent state
/// for it at all (a hookless agent: it is typed into right away, as before the queue).
pub fn at_rest(state: Option<&AgentState>) -> bool {
    matches!(state, None | Some(AgentState::Idle))
}

/// Whether the pane is still in the rest a delivery was scheduled for: same state seq, still
/// idle. `None` on both sides is a pane with no agent state.
fn still_resting(scheduled: Option<u64>, current: Option<(u64, &AgentState)>) -> bool {
    match (scheduled, current) {
        (None, None) => true,
        (Some(seq), Some((now, state))) => seq == now && *state == AgentState::Idle,
        _ => false,
    }
}

pub async fn push(
    state: &ApiState,
    pane_id: &str,
    text: &str,
    by: &str,
) -> Result<(u64, usize), PushError> {
    if text.trim().is_empty() {
        return Err(PushError::Empty);
    }
    if text.len() > MAX_BYTES {
        return Err(PushError::TooLarge);
    }
    let id = state
        .next_queue_id
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let position = {
        let mut queues = state.queues.lock().await;
        let queue = queues.entry(pane_id.to_string()).or_default();
        if queue.items.len() >= MAX_ITEMS {
            return Err(PushError::Full);
        }
        queue.items.push_back(QueueItem {
            id,
            text: text.to_string(),
            by: by.to_string(),
            status: ItemStatus::Queued,
            queued_at: crate::sys::unix_ms(),
        });
        queue.items.len()
    };
    kick(state, pane_id).await;
    Ok((id, position))
}

/// Drops item `id`, or without one the newest item not yet typed. A stalled item can be
/// dropped (that is how the user unblocks the queue); one being typed cannot.
pub async fn cancel(state: &ApiState, pane_id: &str, id: Option<u64>) -> usize {
    let dropped = {
        let mut queues = state.queues.lock().await;
        let Some(queue) = queues.get_mut(pane_id) else {
            return 0;
        };
        let pos = match id {
            Some(id) => queue
                .items
                .iter()
                .position(|i| i.id == id && i.status != ItemStatus::Typed),
            None => queue
                .items
                .iter()
                .rposition(|i| i.status == ItemStatus::Queued),
        };
        let dropped = pos.and_then(|p| queue.items.remove(p)).is_some();
        if queue.items.is_empty() {
            queues.remove(pane_id);
        }
        usize::from(dropped)
    };
    if dropped > 0 {
        kick(state, pane_id).await;
    }
    dropped
}

pub async fn list(state: &ApiState, pane_id: Option<&str>) -> serde_json::Value {
    let queues = state.queues.lock().await;
    let mut map = serde_json::Map::new();
    for (pane, queue) in queues.iter() {
        if pane_id.is_some_and(|p| p != pane) || queue.items.is_empty() {
            continue;
        }
        map.insert(pane.clone(), serde_json::json!(queue.items));
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
pub async fn queued_count(state: &ApiState, pane_id: &str) -> usize {
    state
        .queues
        .lock()
        .await
        .get(pane_id)
        .map_or(0, |q| q.items.len())
}

/// Called by `dispatch_update` for every accepted state change.
pub async fn on_state(state: &ApiState, pane_id: &str, new_state: &AgentState) {
    match new_state {
        AgentState::Closed => {
            state.queues.lock().await.remove(pane_id);
        }
        AgentState::Working => {
            let mut queues = state.queues.lock().await;
            if let Some(queue) = queues.get_mut(pane_id) {
                // The agent took the typed prompt (or, for a stalled one, took it late or the
                // user dealt with the pane).
                if queue
                    .items
                    .front()
                    .is_some_and(|i| i.status != ItemStatus::Queued)
                {
                    queue.items.pop_front();
                }
                if queue.items.is_empty() {
                    queues.remove(pane_id);
                }
            }
        }
        AgentState::Idle => kick(state, pane_id).await,
        _ => {}
    }
}

/// Schedules a delivery when the head is waiting and the pane is at rest now.
async fn kick(state: &ApiState, pane_id: &str) {
    let (seq, ready) = {
        let states = state.pane_states.lock().await;
        match states.get(pane_id) {
            Some(info) => (Some(info.seq_id), at_rest(Some(&info.state))),
            None => (None, true),
        }
    };
    if !ready {
        return;
    }
    {
        let mut queues = state.queues.lock().await;
        let Some(queue) = queues.get_mut(pane_id) else {
            return;
        };
        if !queue
            .items
            .front()
            .is_some_and(|i| i.status == ItemStatus::Queued)
            || queue.scheduled == Some(seq.unwrap_or(0))
        {
            return;
        }
        queue.scheduled = Some(seq.unwrap_or(0));
    }
    schedule(state.clone(), pane_id.to_string(), seq);
}

fn schedule(state: ApiState, pane_id: String, seq: Option<u64>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(REST_MS)).await;
        deliver(&state, &pane_id, seq).await;
    });
}

async fn deliver(state: &ApiState, pane_id: &str, scheduled: Option<u64>) {
    let current = {
        let states = state.pane_states.lock().await;
        states.get(pane_id).map(|i| (i.seq_id, i.state.clone()))
    };
    let resting = still_resting(scheduled, current.as_ref().map(|(seq, st)| (*seq, st)));
    let (id, text) = {
        let mut queues = state.queues.lock().await;
        let Some(queue) = queues.get_mut(pane_id) else {
            return;
        };
        if queue.scheduled == Some(scheduled.unwrap_or(0)) {
            queue.scheduled = None;
        }
        if !resting {
            return;
        }
        let Some(head) = queue.items.front_mut() else {
            return;
        };
        if head.status != ItemStatus::Queued {
            return;
        }
        head.status = ItemStatus::Typed;
        (head.id, head.text.clone())
    };

    let pane = pane_id.to_string();
    let typed = tokio::task::spawn_blocking(move || type_prompt(&pane, &text))
        .await
        .unwrap_or(false);

    if !typed {
        mark_stalled(state, pane_id, id, "could not be typed").await;
        return;
    }

    if current.is_none() {
        // No agent state to confirm the take: done once typed, as before the queue.
        {
            let mut queues = state.queues.lock().await;
            if let Some(queue) = queues.get_mut(pane_id) {
                if queue.items.front().is_some_and(|i| i.id == id) {
                    queue.items.pop_front();
                }
                if queue.items.is_empty() {
                    queues.remove(pane_id);
                }
            }
        }
        kick(state, pane_id).await;
        return;
    }

    let watch = state.clone();
    let pane = pane_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(STALL_MS)).await;
        mark_stalled(&watch, &pane, id, "was typed but the agent did not take it").await;
    });
}

/// Marks item `id` stalled when it is still the typed head, and tells the user.
async fn mark_stalled(state: &ApiState, pane_id: &str, id: u64, why: &str) {
    {
        let mut queues = state.queues.lock().await;
        let Some(head) = queues.get_mut(pane_id).and_then(|q| q.items.front_mut()) else {
            return;
        };
        if head.id != id || head.status != ItemStatus::Typed {
            return;
        }
        head.status = ItemStatus::Stalled;
    }
    tracing::warn!("Queued prompt {id} for pane {pane_id} {why}");
    let _ = crate::tmux_cmd::async_command()
        .args([
            "display-message",
            &format!(
                "󰇮 Queued prompt for {pane_id} {why}: check the pane (waymux queue rm {pane_id})"
            ),
        ])
        .output()
        .await;
}

fn type_prompt(pane_id: &str, text: &str) -> bool {
    use crate::tmux_cmd::{tmux_load_and_paste_bracketed, tmux_status};
    if tmux_load_and_paste_bracketed(Some("waymux_queue"), pane_id, text) {
        return true;
    }
    let flat = text.replace(['\n', '\r'], " ");
    tmux_status(&["send-keys", "-l", "-t", pane_id, "--", &flat])
        && tmux_status(&["send-keys", "-t", pane_id, "Enter"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::PaneStateInfo;

    fn pane(state: AgentState, seq: u64) -> PaneStateInfo {
        PaneStateInfo {
            last_timestamp: 0,
            seq_id: seq,
            state,
            message: None,
            finished_at: 0,
            changed_at: 0,
        }
    }

    #[test]
    fn test_at_rest_only_idle_or_stateless() {
        assert!(at_rest(None));
        assert!(at_rest(Some(&AgentState::Idle)));
        for st in [
            AgentState::Working,
            AgentState::Permission,
            AgentState::AwaitingInput,
            AgentState::Stalled,
            AgentState::Error,
            AgentState::Closed,
        ] {
            assert!(!at_rest(Some(&st)), "{st:?} must not be typed into");
        }
    }

    #[test]
    fn test_still_resting_needs_same_seq_and_idle() {
        assert!(still_resting(None, None));
        assert!(still_resting(Some(3), Some((3, &AgentState::Idle))));
        assert!(!still_resting(Some(3), Some((4, &AgentState::Idle))));
        assert!(!still_resting(Some(3), Some((3, &AgentState::Permission))));
        assert!(!still_resting(None, Some((1, &AgentState::Idle))));
        assert!(!still_resting(Some(1), None));
    }

    #[tokio::test]
    async fn test_push_validates_and_caps() {
        let state = ApiState::new(vec![], 0);
        // Working: held, never scheduled.
        state
            .pane_states
            .lock()
            .await
            .insert("%1".into(), pane(AgentState::Working, 1));
        assert_eq!(push(&state, "%1", "  ", "t").await, Err(PushError::Empty));
        let big = "x".repeat(MAX_BYTES + 1);
        assert_eq!(
            push(&state, "%1", &big, "t").await,
            Err(PushError::TooLarge)
        );
        for n in 1..=MAX_ITEMS {
            assert_eq!(push(&state, "%1", "hi", "t").await.unwrap().1, n);
        }
        assert_eq!(push(&state, "%1", "hi", "t").await, Err(PushError::Full));
        assert_eq!(queued_count(&state, "%1").await, MAX_ITEMS);
    }

    #[tokio::test]
    async fn test_permission_holds_and_working_pops_only_typed_head() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%2".into(), pane(AgentState::Permission, 1));
        push(&state, "%2", "first", "t").await.unwrap();
        push(&state, "%2", "second", "t").await.unwrap();

        // Nothing scheduled while the pane is on a prompt.
        assert!(state.queues.lock().await["%2"].scheduled.is_none());

        // Working with an untyped head pops nothing.
        on_state(&state, "%2", &AgentState::Working).await;
        assert_eq!(queued_count(&state, "%2").await, 2);

        // A typed head is popped when the agent works.
        state.queues.lock().await.get_mut("%2").unwrap().items[0].status = ItemStatus::Typed;
        on_state(&state, "%2", &AgentState::Working).await;
        let list = list(&state, Some("%2")).await;
        assert_eq!(list["%2"][0]["text"], "second");
        assert_eq!(list["%2"][0]["status"], "queued");
    }

    #[tokio::test]
    async fn test_idle_schedules_once_per_seq() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%3".into(), pane(AgentState::Idle, 7));
        push(&state, "%3", "go", "t").await.unwrap();
        assert_eq!(state.queues.lock().await["%3"].scheduled, Some(7));
        // A second kick for the same rest does not schedule again.
        on_state(&state, "%3", &AgentState::Idle).await;
        assert_eq!(state.queues.lock().await["%3"].scheduled, Some(7));
    }

    #[tokio::test]
    async fn test_delivery_skipped_when_rest_was_broken() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%4".into(), pane(AgentState::Working, 9));
        push(&state, "%4", "later", "t").await.unwrap();
        // Scheduled for seq 8, but the pane moved on to seq 9 (working): nothing is typed.
        deliver(&state, "%4", Some(8)).await;
        let list = list(&state, Some("%4")).await;
        assert_eq!(list["%4"][0]["status"], "queued");
    }

    #[tokio::test]
    async fn test_cancel_newest_queued_and_stalled_by_id() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%5".into(), pane(AgentState::Working, 1));
        let (a, _) = push(&state, "%5", "a", "t").await.unwrap();
        push(&state, "%5", "b", "t").await.unwrap();
        assert_eq!(cancel(&state, "%5", None).await, 1);
        assert_eq!(list(&state, Some("%5")).await["%5"][0]["text"], "a");

        state.queues.lock().await.get_mut("%5").unwrap().items[0].status = ItemStatus::Typed;
        assert_eq!(
            cancel(&state, "%5", Some(a)).await,
            0,
            "typed item cannot be dropped"
        );
        state.queues.lock().await.get_mut("%5").unwrap().items[0].status = ItemStatus::Stalled;
        assert_eq!(cancel(&state, "%5", Some(a)).await, 1);
        assert_eq!(queued_count(&state, "%5").await, 0);
    }

    #[tokio::test]
    async fn test_closed_drops_queue() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%6".into(), pane(AgentState::Working, 1));
        push(&state, "%6", "x", "t").await.unwrap();
        on_state(&state, "%6", &AgentState::Closed).await;
        assert_eq!(queued_count(&state, "%6").await, 0);
    }
}
