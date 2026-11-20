//! Keep the machine awake while agents work (`keep_awake`, on by default).
//!
//! While any pane is `working` or `background`, the daemon holds a `systemd-inhibit
//! --what=idle:sleep` lock, so a laptop does not suspend in the middle of a turn; the lock goes
//! when the last one rests. The lock's program is `cat` reading a pipe the daemon holds, so it
//! also goes when the daemon exits or crashes. Where `systemd-inhibit` is missing (macOS, a
//! host without logind), it says so once and does nothing. Idea from TUIOS `main` (MIT).

use std::io::Write as _;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::adapters::AgentState;

/// The states that hold the lock: the agent is busy with nothing for the human to do.
pub fn holds_lock(state: &AgentState) -> bool {
    matches!(state, AgentState::Working | AgentState::Background)
}

/// Starts the lock's process; tests replace it.
pub type Spawner = fn() -> std::io::Result<Child>;

pub struct KeepAwake {
    lock: Mutex<Inner>,
    spawn: Spawner,
}

#[derive(Default)]
struct Inner {
    child: Option<Child>,
    warned: bool,
}

impl Default for KeepAwake {
    /// The real inhibitor; unit tests get a stand-in so a test never holds the host awake.
    fn default() -> Self {
        #[cfg(not(test))]
        return Self::with_spawner(spawn_inhibit);
        #[cfg(test)]
        return Self::with_spawner(tests::fake);
    }
}

impl KeepAwake {
    pub fn with_spawner(spawn: Spawner) -> Self {
        Self {
            lock: Mutex::new(Inner::default()),
            spawn,
        }
    }

    /// Takes the lock when `busy`, releases it otherwise. Cheap when nothing changes.
    pub fn set(&self, busy: bool) {
        let Ok(mut inner) = self.lock.lock() else {
            return;
        };
        // A lock whose process died (logind restarted, killed by hand) is taken again.
        if let Some(child) = inner.child.as_mut()
            && !matches!(child.try_wait(), Ok(None))
        {
            inner.child = None;
        }
        match (busy, inner.child.is_some()) {
            (true, false) => match (self.spawn)() {
                Ok(child) => {
                    tracing::info!("Agents working: holding a sleep and idle inhibitor");
                    inner.child = Some(child);
                }
                Err(e) => {
                    if !inner.warned {
                        tracing::warn!("keep_awake: cannot start systemd-inhibit: {e}");
                        inner.warned = true;
                    }
                }
            },
            (false, true) => {
                if let Some(mut child) = inner.child.take() {
                    release(&mut child);
                    tracing::info!("No agent working: sleep and idle inhibitor released");
                }
            }
            _ => {}
        }
    }

    pub fn held(&self) -> bool {
        self.lock.lock().is_ok_and(|i| i.child.is_some())
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Ok(inner) = self.lock.get_mut()
            && let Some(mut child) = inner.child.take()
        {
            release(&mut child);
        }
    }
}

fn release(child: &mut Child) {
    // Closing the pipe ends `cat`, which ends the inhibitor; kill covers a stuck one.
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg_attr(test, allow(dead_code))]
fn spawn_inhibit() -> std::io::Result<Child> {
    let mut child = Command::new("systemd-inhibit")
        .args([
            "--what=idle:sleep",
            "--who=waymux",
            "--why=AI agents are working",
            "--mode=block",
            "cat",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Touch the pipe so a broken one shows now rather than at release.
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.flush();
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fake() -> std::io::Result<Child> {
        Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
    }

    fn missing() -> std::io::Result<Child> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "none"))
    }

    #[test]
    fn test_holds_lock_only_for_busy_states() {
        assert!(holds_lock(&AgentState::Working));
        assert!(holds_lock(&AgentState::Background));
        for s in [
            AgentState::Idle,
            AgentState::AwaitingInput,
            AgentState::Permission,
            AgentState::Stalled,
            AgentState::Error,
            AgentState::Closed,
        ] {
            assert!(!holds_lock(&s), "{s:?}");
        }
    }

    #[test]
    fn test_set_takes_once_and_releases() {
        let k = KeepAwake::with_spawner(fake);
        assert!(!k.held());
        k.set(true);
        assert!(k.held());
        let pid = k.lock.lock().unwrap().child.as_ref().unwrap().id();
        k.set(true);
        assert_eq!(
            k.lock.lock().unwrap().child.as_ref().unwrap().id(),
            pid,
            "a second busy pane does not start a second lock"
        );
        k.set(false);
        assert!(!k.held());
        k.set(false);
        assert!(!k.held());
    }

    #[test]
    fn test_dead_lock_is_taken_again() {
        let k = KeepAwake::with_spawner(fake);
        k.set(true);
        {
            let mut inner = k.lock.lock().unwrap();
            let child = inner.child.as_mut().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let old = k.lock.lock().unwrap().child.as_ref().unwrap().id();
        k.set(true);
        assert!(k.held());
        assert_ne!(k.lock.lock().unwrap().child.as_ref().unwrap().id(), old);
    }

    #[test]
    fn test_missing_inhibitor_is_a_no_op() {
        let k = KeepAwake::with_spawner(missing);
        k.set(true);
        k.set(true);
        assert!(!k.held());
        assert!(k.lock.lock().unwrap().warned);
    }
}
