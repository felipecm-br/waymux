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
//! user drops it with `waymux queue rm`. Halfway there, a prompt whose end still shows on the
//! bottom of a resting pane gets one more `Enter` (a TUI busy taking in a long paste can drop
//! the first); the text itself is never sent again.
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
/// When a typed prompt still unsent on screen gets one more `Enter` (see `needs_enter_again`).
pub const ENTER_RETRY_MS: u64 = 1500;
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
    persist(state).await;
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
        persist(state).await;
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
    {
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
    }
    persist(state).await;
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
    persist(state).await;
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
            let had = state.queues.lock().await.remove(pane_id).is_some();
            if had {
                persist(state).await;
            }
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
    // Out of the saved file before it is typed: a restart in between never types it twice.
    persist(state).await;

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
        tokio::time::sleep(std::time::Duration::from_millis(ENTER_RETRY_MS)).await;
        if needs_enter_again(&watch, &pane, id).await {
            let target = pane.clone();
            let pressed = tokio::task::spawn_blocking(move || {
                crate::tmux_cmd::tmux_status(&["send-keys", "-t", &target, "Enter"])
            })
            .await
            .unwrap_or(false);
            if pressed {
                tracing::info!("Queued prompt {id} for pane {pane}: Enter pressed again");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(STALL_MS - ENTER_RETRY_MS)).await;
        mark_stalled(&watch, &pane, id, "was typed but the agent did not take it").await;
    });
}

/// Saves the prompts not typed yet (see `super::queue_store`) when the daemon keeps a file and
/// they changed. The lock on the last snapshot is held through the write, so two saves never land
/// out of order.
pub async fn persist(state: &ApiState) {
    let Some(path) = state.queue_file.clone() else {
        return;
    };
    let snap = super::queue_store::snapshot(&*state.queues.lock().await);
    let mut last = state.queue_saved.lock().await;
    if last.as_ref() == Some(&snap) {
        return;
    }
    let to_write = snap.clone();
    let written = tokio::task::spawn_blocking(move || {
        let pid = super::queue_store::tmux_pid()?;
        let json = super::queue_store::encode(&pid, &to_write);
        match crate::auth::write_token_file(&path, &json) {
            Ok(()) => Some(()),
            Err(e) => {
                tracing::warn!("cannot save the queue to {}: {e}", path.display());
                None
            }
        }
    })
    .await
    .ok()
    .flatten();
    if written.is_some() {
        *last = Some(snap);
    }
}

/// Puts back the prompts an earlier daemon saved for panes of this tmux server, then delivers
/// them as usual (a pane at rest gets its head after the rest window). Returns how many.
pub async fn restore(state: &ApiState) -> usize {
    let Some(path) = state.queue_file.clone() else {
        return 0;
    };
    let saved = tokio::task::spawn_blocking(move || {
        let json = std::fs::read_to_string(&*path).ok()?;
        let pid = super::queue_store::tmux_pid()?;
        Some(super::queue_store::decode_for(
            &json,
            &pid,
            &super::queue_store::live_panes(),
        ))
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    restore_saved(state, saved).await
}

async fn restore_saved(state: &ApiState, saved: super::queue_store::SavedPanes) -> usize {
    let mut count = 0;
    let mut max_id = 0;
    {
        let mut queues = state.queues.lock().await;
        for (pane, items) in &saved {
            let queue = queues.entry(pane.clone()).or_default();
            for i in items {
                if queue.items.len() >= MAX_ITEMS || queue.items.iter().any(|q| q.id == i.id) {
                    continue;
                }
                max_id = max_id.max(i.id);
                queue.items.push_back(QueueItem {
                    id: i.id,
                    text: i.text.clone(),
                    by: i.by.clone(),
                    status: ItemStatus::Queued,
                    queued_at: i.queued_at,
                    force: false,
                });
                count += 1;
            }
        }
    }
    state
        .next_queue_id
        .fetch_max(max_id + 1, std::sync::atomic::Ordering::SeqCst);
    for pane in saved.keys() {
        kick(state, pane).await;
    }
    if count > 0 {
        tracing::info!("Restored {count} queued prompt(s) from the last daemon");
    }
    count
}

/// Whether the typed head `id` should get one more `Enter`: the agent has not shown `working`
/// (the head would be gone), the pane still rests, and the prompt's end is still on the bottom
/// of the screen. A TUI can drop the `Enter` that follows a long bracketed paste while it is
/// still taking the paste in (opencode does; Orca resends it, stablyai/orca#45850). Only `Enter`
/// is sent, never the text, so a prompt is still typed at most once.
async fn needs_enter_again(state: &ApiState, pane_id: &str, id: u64) -> bool {
    let text = {
        let queues = state.queues.lock().await;
        let Some(head) = queues.get(pane_id).and_then(|q| q.items.front()) else {
            return false;
        };
        if head.id != id || head.status != ItemStatus::Typed {
            return false;
        }
        head.text.clone()
    };
    let resting = {
        let states = state.pane_states.lock().await;
        states
            .get(pane_id)
            .is_some_and(super::state::PaneStateInfo::at_rest)
    };
    if !resting {
        return false;
    }
    let probe = state.unsent_probe.clone();
    let pane = pane_id.to_string();
    tokio::task::spawn_blocking(move || probe(&pane, &text))
        .await
        .unwrap_or(false)
}

/// How many characters of the prompt's end must still show on the bottom of the pane.
const TAIL_CHARS: usize = 24;
/// How many non-empty screen lines from the bottom hold the composer.
const COMPOSER_LINES: usize = 12;

/// The prompt's last characters, with whitespace and frame drawing removed (a composer wraps
/// and boxes its text). `None` for a prompt too short to tell apart from the screen around it.
fn prompt_tail(text: &str) -> Option<String> {
    let flat = squash(text);
    let n = flat.chars().count();
    if n < 8 {
        return None;
    }
    Some(flat.chars().skip(n.saturating_sub(TAIL_CHARS)).collect())
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !('\u{2500}'..='\u{259f}').contains(c))
        .collect()
}

/// Whether `screen`'s bottom lines still show the end of `text`.
fn tail_on_screen(screen: &str, text: &str) -> bool {
    let Some(tail) = prompt_tail(text) else {
        return false;
    };
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let bottom = squash(&lines[lines.len().saturating_sub(COMPOSER_LINES)..].join(""));
    bottom.contains(&tail)
}

/// The live check: the pane's visible screen, without colors.
pub fn prompt_still_unsent(pane: &str, text: &str) -> bool {
    let Ok(out) = crate::tmux_cmd::command()
        .args(["capture-pane", "-p", "-t", pane])
        .output()
    else {
        return false;
    };
    out.status.success() && tail_on_screen(&String::from_utf8_lossy(&out.stdout), text)
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
        assert_eq!(MAX_BYTES, 16384);
        let state = ApiState::new(vec![], 0);
        // Working: held, never scheduled.
        state
            .pane_states
            .lock()
            .await
            .insert("%1".into(), pane(AgentState::Working, 1));
        assert_eq!(push(&state, "%1", "  ", "t").await, Err(PushError::Empty));
        let exact = "x".repeat(MAX_BYTES);
        assert_eq!(push(&state, "%1", &exact, "t").await.unwrap().1, 1);
        let big = "x".repeat(MAX_BYTES + 1);
        assert_eq!(
            push(&state, "%1", &big, "t").await,
            Err(PushError::TooLarge)
        );
        for n in 2..=MAX_ITEMS {
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

        // When pushed while Working, scheduled is None until on_state(Idle) kicks it
        let s_work = ApiState::new(vec![], 0);
        s_work
            .pane_states
            .lock()
            .await
            .insert("%w1".into(), pane(AgentState::Working, 1));
        push(&s_work, "%w1", "after work", "t").await.unwrap();
        assert_eq!(s_work.queues.lock().await["%w1"].scheduled, None);
        s_work
            .pane_states
            .lock()
            .await
            .insert("%w1".into(), pane(AgentState::Idle, 2));
        on_state(&s_work, "%w1", &AgentState::Idle).await;
        assert_eq!(s_work.queues.lock().await["%w1"].scheduled, Some(2));
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

        // Cancel a stalled head kicks delivery of next queued item on an idle pane
        let s2 = ApiState::new(vec![], 0);
        s2.pane_states
            .lock()
            .await
            .insert("%8".into(), pane(AgentState::Idle, 1));
        let (id1, _) = push(&s2, "%8", "first", "t").await.unwrap();
        let (_id2, _) = push(&s2, "%8", "second", "t").await.unwrap();
        s2.queues.lock().await.get_mut("%8").unwrap().items[0].status = ItemStatus::Stalled;
        s2.queues.lock().await.get_mut("%8").unwrap().scheduled = None;

        // Cancel with non-existent id drops 0 and does NOT kick/schedule
        assert_eq!(cancel(&s2, "%8", Some(9999)).await, 0);
        assert_eq!(s2.queues.lock().await["%8"].scheduled, None);

        // on_state Idle does NOT schedule when head is Stalled
        on_state(&s2, "%8", &AgentState::Idle).await;
        assert_eq!(s2.queues.lock().await["%8"].scheduled, None);

        assert_eq!(cancel(&s2, "%8", Some(id1)).await, 1);
        assert_eq!(s2.queues.lock().await["%8"].scheduled, Some(1));
    }

    #[tokio::test]
    async fn test_list_filters_panes_and_skips_empty() {
        let state = ApiState::new(vec![], 0);
        push(&state, "%p1", "msg1", "t").await.unwrap();
        let (id2, _) = push(&state, "%p2", "msg2", "t").await.unwrap();

        let l_all = list(&state, None).await;
        assert!(l_all.get("%p1").is_some());
        assert!(l_all.get("%p2").is_some());

        let l1 = list(&state, Some("%p1")).await;
        assert!(l1.get("%p1").is_some());
        assert!(l1.get("%p2").is_none());

        let l_none = list(&state, Some("%nonexistent")).await;
        assert!(l_none.as_object().unwrap().is_empty());

        // mark_stalled checks both id and Typed status
        mark_stalled(&state, "%p2", id2, "stalled").await;
        assert_eq!(
            state.queues.lock().await["%p2"].items[0].status,
            ItemStatus::Queued
        );
        state.queues.lock().await.get_mut("%p2").unwrap().items[0].status = ItemStatus::Typed;
        mark_stalled(&state, "%p2", 999_999, "stalled").await;
        assert_eq!(
            state.queues.lock().await["%p2"].items[0].status,
            ItemStatus::Typed
        );
        mark_stalled(&state, "%p2", id2, "stalled").await;
        assert_eq!(
            state.queues.lock().await["%p2"].items[0].status,
            ItemStatus::Stalled
        );
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

    #[tokio::test]
    async fn test_deliver_on_rest_marks_stalled_when_tmux_cannot_type() {
        let state = ApiState::new(vec![], 0);
        state
            .pane_states
            .lock()
            .await
            .insert("%d1".into(), pane(AgentState::Idle, 1));
        push(&state, "%d1", "prompt text", "t").await.unwrap();
        assert_eq!(state.queues.lock().await["%d1"].scheduled, Some(1));
        deliver(&state, "%d1", Some(1)).await;
        // deliver clears scheduled and marks head as Stalled since type_prompt failed
        assert_eq!(state.queues.lock().await["%d1"].scheduled, None);
        assert_eq!(
            state.queues.lock().await["%d1"].items[0].status,
            ItemStatus::Stalled
        );

        // deliver for a pane with no agent state (current is None)
        let s_no_agent = ApiState::new(vec![], 0);
        push(&s_no_agent, "%no_agent", "text", "t").await.unwrap();
        deliver(&s_no_agent, "%no_agent", None).await;
    }

    #[test]
    fn test_tail_on_screen_reads_through_wraps_and_frames() {
        let prompt = "Review src/auth.rs and list every bug you find";
        // opencode-like box, the prompt wrapped over two rows.
        let screen = "history line\n\n┃ Review src/auth.rs and list ev ┃\n┃ ery bug you find               ┃\n┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\n  Build  gpt-6\n";
        assert!(tail_on_screen(screen, prompt));
        // Sent: the composer is empty and the prompt scrolled out of the bottom lines.
        let mut sent = format!("› {prompt}\n");
        for i in 0..20 {
            sent += &["tool output ", &i.to_string(), "\n"].concat();
        }
        sent.push_str("┃                                ┃\n");
        assert!(!tail_on_screen(&sent, prompt));
        // Too short to tell apart from the screen: never pressed.
        assert!(!tail_on_screen("› go\n", "go"));
        assert_eq!(prompt_tail("short"), None);
        assert_eq!(prompt_tail(&"x".repeat(40)).unwrap().len(), TAIL_CHARS);
    }

    fn with_unsent(unsent: bool) -> ApiState {
        let mut state = ApiState::new(vec![], 0);
        state.unsent_probe = std::sync::Arc::new(move |_, _| unsent);
        state
    }

    #[tokio::test]
    async fn test_needs_enter_again_only_for_a_resting_typed_head_still_on_screen() {
        let state = with_unsent(true);
        state
            .pane_states
            .lock()
            .await
            .insert("%e1".into(), pane(AgentState::Idle, 1));
        let (id, _) = push(&state, "%e1", "a prompt long enough", "t")
            .await
            .unwrap();
        // Not typed yet: nothing to press.
        assert!(!needs_enter_again(&state, "%e1", id).await);
        state.queues.lock().await.get_mut("%e1").unwrap().items[0].status = ItemStatus::Typed;
        assert!(needs_enter_again(&state, "%e1", id).await);
        // Another id, or a stalled head: no.
        assert!(!needs_enter_again(&state, "%e1", id + 99).await);
        // The pane left rest (a permission prompt): no Enter into it.
        state
            .pane_states
            .lock()
            .await
            .insert("%e1".into(), pane(AgentState::Permission, 2));
        assert!(!needs_enter_again(&state, "%e1", id).await);
        // Working took the head: nothing left to press.
        on_state(&state, "%e1", &AgentState::Working).await;
        assert!(!needs_enter_again(&state, "%e1", id).await);

        // The prompt is gone from the screen: no.
        let gone = with_unsent(false);
        gone.pane_states
            .lock()
            .await
            .insert("%e2".into(), pane(AgentState::Idle, 1));
        let (id2, _) = push(&gone, "%e2", "a prompt long enough", "t")
            .await
            .unwrap();
        gone.queues.lock().await.get_mut("%e2").unwrap().items[0].status = ItemStatus::Typed;
        assert!(!needs_enter_again(&gone, "%e2", id2).await);
    }

    #[tokio::test]
    async fn test_restore_saved_puts_prompts_back_in_order_and_moves_ids_on() {
        use super::super::queue_store::{SavedItem, SavedPanes};
        let state = ApiState::new(vec![], 0);
        // A busy pane: restored prompts wait, nothing is typed into a turn.
        state
            .pane_states
            .lock()
            .await
            .insert("%r1".into(), pane(AgentState::Working, 1));
        let one = |id, t: &str| SavedItem {
            id,
            text: t.into(),
            by: "inject".into(),
            queued_at: 5,
        };
        let saved = SavedPanes::from([("%r1".into(), vec![one(41, "first"), one(42, "second")])]);
        assert_eq!(restore_saved(&state, saved.clone()).await, 2);
        let l = list(&state, Some("%r1")).await;
        assert_eq!(l["%r1"][0]["text"], "first");
        assert_eq!(l["%r1"][1]["text"], "second");
        assert_eq!(l["%r1"][0]["status"], "queued");
        assert_eq!(l["%r1"][0]["by"], "inject");
        // Restoring twice does not double them; new pushes get ids past the saved ones.
        assert_eq!(restore_saved(&state, saved).await, 0);
        let (id, pos) = push(&state, "%r1", "third", "t").await.unwrap();
        assert!(id > 42);
        assert_eq!(pos, 3);
        // Memory-only state (no file): persist and restore are no-ops.
        persist(&state).await;
        assert_eq!(restore(&state).await, 0);
    }
}
