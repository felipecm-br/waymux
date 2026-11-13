//! Delivery queue: prompts for an agent pane are held in the daemon and typed only once the
//! agent has been at rest for a second, one per rest, never into a permission prompt or the
//! middle of a turn.
//!
//! A pane the daemon has no agent state for (a hookless agent) is typed into at once, as
//! before the queue.
//!
//! A typed prompt stays at the head until the pane shows `working` (the agent took it). One
//! the agent does not take within the stall window is marked `stalled` and never typed again,
//! so text is never typed twice; it holds the ones behind it until the pane next works or the
//! user drops it with `waymux queue rm`.
//!
//! A prompt is never typed into a half-written one: when the pane's composer holds a draft at
//! delivery time, the head is `held` and checked again while the pane rests. Sending or stashing
//! the draft lets it go; `queue/send` types it at once over the hold.

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
/// How often a held head looks at the composer again while the pane stays at rest.
pub const HOLD_POLL_MS: u64 = 1500;
/// Checks per rest before a held head waits for the next state change instead (10 minutes).
pub const HOLD_MAX_POLLS: u32 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Queued,
    /// Waiting: the user has a draft in the composer.
    Held,
    Typed,
    Stalled,
}

impl ItemStatus {
    /// Not typed yet: may be delivered, edited, sent now or dropped.
    pub fn waiting(self) -> bool {
        matches!(self, ItemStatus::Queued | ItemStatus::Held)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub id: u64,
    pub text: String,
    pub by: String,
    pub status: ItemStatus,
    pub queued_at: u64,
    /// `queue/send`: typed over a draft hold.
    #[serde(skip)]
    pub force: bool,
}

#[derive(Debug, Default)]
pub struct PaneQueue {
    pub items: VecDeque<QueueItem>,
    /// The pane state seq a delivery is already scheduled for (0 for a pane with no agent
    /// state; seqs start at 1), so a burst of updates schedules one delivery.
    scheduled: Option<u64>,
    /// Composer checks the held head has made in this rest.
    held_polls: u32,
}

pub type Queues = HashMap<String, PaneQueue>;

#[derive(Debug, PartialEq, Eq)]
pub enum PushError {
    Empty,
    TooLarge,
    Full,
    /// No waiting item with that id (typed, stalled or gone).
    NotFound,
}

impl PushError {
    pub fn code(&self) -> &'static str {
        match self {
            PushError::Empty => "empty",
            PushError::TooLarge => "too_large",
            PushError::Full => "queue_full",
            PushError::NotFound => "not_found",
        }
    }
}

/// Whether the pane is still in the rest a delivery was scheduled for: same state seq, still
/// at rest. `None` on both sides is a pane with no agent state.
fn still_resting(scheduled: Option<u64>, current: Option<(u64, bool)>) -> bool {
    match (scheduled, current) {
        (None, None) => true,
        (Some(seq), Some((now, rest))) => seq == now && rest,
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
            force: false,
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
            None => queue.items.iter().rposition(|i| i.status.waiting()),
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

/// Replaces the text of a prompt not typed yet.
pub async fn edit(state: &ApiState, pane_id: &str, id: u64, text: &str) -> Result<(), PushError> {
    if text.trim().is_empty() {
        return Err(PushError::Empty);
    }
    if text.len() > MAX_BYTES {
        return Err(PushError::TooLarge);
    }
    let mut queues = state.queues.lock().await;
    let item = queues
        .get_mut(pane_id)
        .and_then(|q| {
            q.items
                .iter_mut()
                .find(|i| i.id == id && i.status.waiting())
        })
        .ok_or(PushError::NotFound)?;
    item.text = text.to_string();
    Ok(())
}

/// Moves a waiting prompt (`id`, else the head) to the front and types it over any draft hold:
/// at once when the pane is at rest (`true`), else as soon as it is (`false`). Never ahead of a
/// prompt being typed.
pub async fn send_now(state: &ApiState, pane_id: &str, id: Option<u64>) -> Result<bool, PushError> {
    let (seq, ready) = {
        let states = state.pane_states.lock().await;
        match states.get(pane_id) {
            Some(info) => (Some(info.seq_id), info.at_rest()),
            None => (None, true),
        }
    };
    {
        let mut queues = state.queues.lock().await;
        let queue = queues.get_mut(pane_id).ok_or(PushError::NotFound)?;
        if queue.items.front().is_some_and(|i| !i.status.waiting()) {
            return Err(PushError::NotFound);
        }
        let pos = match id {
            Some(id) => queue
                .items
                .iter()
                .position(|i| i.id == id && i.status.waiting()),
            None => (!queue.items.is_empty()).then_some(0),
        }
        .ok_or(PushError::NotFound)?;
        let mut item = queue.items.remove(pos).ok_or(PushError::NotFound)?;
        item.force = true;
        item.status = ItemStatus::Queued;
        queue.items.push_front(item);
        if ready {
            queue.scheduled = Some(seq.unwrap_or(0));
        }
    }
    if ready {
        deliver(state, pane_id, seq).await;
    }
    Ok(ready)
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
                if queue.items.front().is_some_and(|i| !i.status.waiting()) {
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
            Some(info) => (Some(info.seq_id), info.at_rest()),
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
        if !queue.items.front().is_some_and(|i| i.status.waiting())
            || queue.scheduled == Some(seq.unwrap_or(0))
        {
            return;
        }
        queue.scheduled = Some(seq.unwrap_or(0));
        queue.held_polls = 0;
    }
    schedule(state.clone(), pane_id.to_string(), seq);
}

fn schedule(state: ApiState, pane_id: String, seq: Option<u64>) {
    schedule_after(state, pane_id, seq, REST_MS);
}

fn schedule_after(state: ApiState, pane_id: String, seq: Option<u64>, ms: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        deliver(&state, &pane_id, seq).await;
    });
}

async fn deliver(state: &ApiState, pane_id: &str, scheduled: Option<u64>) {
    let current = {
        let states = state.pane_states.lock().await;
        states.get(pane_id).map(|i| (i.seq_id, i.at_rest()))
    };
    let resting = still_resting(scheduled, current);
    let (id, force) = {
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
        let Some(head) = queue.items.front() else {
            return;
        };
        if !head.status.waiting() {
            return;
        }
        (head.id, head.force)
    };

    // A hookless pane has no composer we know how to read; an agent's is checked for a draft.
    if !force && current.is_some() {
        let probe = state.draft_probe.clone();
        let pane = pane_id.to_string();
        let draft = tokio::task::spawn_blocking(move || probe(&pane))
            .await
            .unwrap_or(false);
        if draft {
            hold(state, pane_id, id, scheduled).await;
            return;
        }
    }

    let text = {
        let mut queues = state.queues.lock().await;
        let Some(head) = queues.get_mut(pane_id).and_then(|q| q.items.front_mut()) else {
            return;
        };
        if head.id != id || !head.status.waiting() {
            return;
        }
        head.status = ItemStatus::Typed;
        head.text.clone()
    };
    if let Some(queue) = state.queues.lock().await.get_mut(pane_id) {
        queue.held_polls = 0;
    }

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

/// The user is mid-draft: mark the head held, say so once, and look again while the pane rests.
/// The poll owns the rest's schedule slot, so a push or kick in the meantime does not start a
/// second one; a state change (the user sent the draft) schedules afresh.
async fn hold(state: &ApiState, pane_id: &str, id: u64, scheduled: Option<u64>) {
    let first = {
        let mut queues = state.queues.lock().await;
        let Some(queue) = queues.get_mut(pane_id) else {
            return;
        };
        let Some(head) = queue
            .items
            .front_mut()
            .filter(|h| h.id == id && h.status.waiting())
        else {
            return;
        };
        head.status = ItemStatus::Held;
        queue.held_polls += 1;
        if queue.held_polls > HOLD_MAX_POLLS {
            return;
        }
        if queue.scheduled.is_some() {
            // A newer rest already has its own delivery.
            return;
        }
        queue.scheduled = Some(scheduled.unwrap_or(0));
        queue.held_polls == 1
    };
    if first {
        tracing::info!("Queued prompt {id} for pane {pane_id} held: draft in the composer");
        let _ = crate::tmux_cmd::async_command()
            .args([
                "display-message",
                &format!(
                    "󰇮 Prompt held for {pane_id}: you have a draft there; it goes once you send or stash it (waymux queue send {pane_id})"
                ),
            ])
            .output()
            .await;
    }
    schedule_after(state.clone(), pane_id.to_string(), scheduled, HOLD_POLL_MS);
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
    fn test_at_rest_only_idle() {
        assert!(pane(AgentState::Idle, 1).at_rest());
        for st in [
            AgentState::Working,
            AgentState::Permission,
            AgentState::AwaitingInput,
            AgentState::Stalled,
            AgentState::Error,
            AgentState::Closed,
        ] {
            assert!(
                !pane(st.clone(), 1).at_rest(),
                "{st:?} must not be typed into"
            );
        }
    }

    #[test]
    fn test_still_resting_needs_same_seq_and_idle() {
        assert!(still_resting(None, None));
        assert!(still_resting(Some(3), Some((3, true))));
        assert!(!still_resting(Some(3), Some((4, true))));
        assert!(!still_resting(Some(3), Some((3, false))));
        assert!(!still_resting(None, Some((1, true))));
        assert!(!still_resting(Some(1), None));
    }

    #[tokio::test]
    async fn test_focus_dismissed_prompt_is_not_a_rest() {
        let state = ApiState::new(vec![], 0);
        let mut info = pane(AgentState::Idle, 2);
        info.message = Some(crate::api::state::DISMISSED.into());
        assert!(!info.at_rest());
        state.pane_states.lock().await.insert("%7".into(), info);
        push(&state, "%7", "note", "t").await.unwrap();
        // Nothing scheduled: the agent may still be on its permission prompt.
        assert!(state.queues.lock().await["%7"].scheduled.is_none());
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

    fn with_draft(draft: bool) -> ApiState {
        let mut state = ApiState::new(vec![], 0);
        state.draft_probe = std::sync::Arc::new(move |_| draft);
        state
    }

    #[tokio::test]
    async fn test_draft_in_composer_holds_the_head() {
        let state = with_draft(true);
        state
            .pane_states
            .lock()
            .await
            .insert("%8".into(), pane(AgentState::Idle, 4));
        push(&state, "%8", "review notes", "t").await.unwrap();
        deliver(&state, "%8", Some(4)).await;
        let l = list(&state, Some("%8")).await;
        assert_eq!(l["%8"][0]["status"], "held", "never typed over a draft");
        // The hold owns the rest's slot: a second push does not schedule another delivery.
        assert_eq!(state.queues.lock().await["%8"].scheduled, Some(4));
        assert_eq!(state.queues.lock().await["%8"].held_polls, 1);
        // Working (the user sent the draft) keeps the held prompt: it was never typed.
        on_state(&state, "%8", &AgentState::Working).await;
        assert_eq!(queued_count(&state, "%8").await, 1);
    }

    #[tokio::test]
    async fn test_hold_releases_when_the_composer_empties() {
        let state = with_draft(false);
        state
            .pane_states
            .lock()
            .await
            .insert("%9".into(), pane(AgentState::Idle, 2));
        push(&state, "%9", "go", "t").await.unwrap();
        state.queues.lock().await.get_mut("%9").unwrap().items[0].status = ItemStatus::Held;
        deliver(&state, "%9", Some(2)).await;
        // Typed (tmux has no server in tests, so it is marked stalled): no longer held.
        let l = list(&state, Some("%9")).await;
        assert_ne!(l["%9"][0]["status"], "held");
        assert_ne!(l["%9"][0]["status"], "queued");
    }

    #[tokio::test]
    async fn test_send_now_moves_to_front_and_bypasses_the_hold() {
        let state = with_draft(true);
        state
            .pane_states
            .lock()
            .await
            .insert("%10".into(), pane(AgentState::Working, 1));
        push(&state, "%10", "a", "t").await.unwrap();
        let (b, _) = push(&state, "%10", "b", "t").await.unwrap();
        // Busy: b goes first, typed as soon as the agent rests, over any draft.
        assert_eq!(send_now(&state, "%10", Some(b)).await, Ok(false));
        let l = list(&state, Some("%10")).await;
        assert_eq!(l["%10"][0]["text"], "b");
        assert!(state.queues.lock().await["%10"].items[0].force);
        // At rest: typed now even with a draft (stalled here: no tmux server).
        state
            .pane_states
            .lock()
            .await
            .insert("%10".into(), pane(AgentState::Idle, 2));
        assert_eq!(send_now(&state, "%10", Some(b)).await, Ok(true));
        assert_ne!(list(&state, Some("%10")).await["%10"][0]["status"], "held");
        assert_eq!(
            send_now(&state, "%10", Some(999)).await,
            Err(PushError::NotFound)
        );
    }

    #[tokio::test]
    async fn test_edit_only_waiting_items() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%11".into(), pane(AgentState::Working, 1));
        let (a, _) = push(&state, "%11", "old", "t").await.unwrap();
        edit(&state, "%11", a, "new").await.unwrap();
        assert_eq!(list(&state, Some("%11")).await["%11"][0]["text"], "new");
        assert_eq!(edit(&state, "%11", a, " ").await, Err(PushError::Empty));
        state.queues.lock().await.get_mut("%11").unwrap().items[0].status = ItemStatus::Typed;
        assert_eq!(
            edit(&state, "%11", a, "late").await,
            Err(PushError::NotFound)
        );
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
