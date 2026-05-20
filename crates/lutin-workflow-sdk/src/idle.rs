//! Connection-count-based idle shutdown for workflow engines.
//!
//! Each workflow container hosts a single session and outlives a
//! disconnect from the desktop (the control panel no longer kills
//! containers on shutdown). To bound resource use, workflows take a
//! [`ConnectionGuard`] for every accepted WebSocket connection and
//! spawn [`wait_until_idle`] alongside their accept loop; once the
//! guard count has been continuously zero for `idle_timeout`, the
//! future resolves and the workflow exits.
//!
//! "Idle" here means "no connected clients" — not "no in-flight
//! work". A turn that's still running while the desktop is closed
//! will keep going; the idle countdown only starts when both turns
//! and clients are absent, because turns hold no guards. Workflows
//! that want to keep the container alive during long background work
//! should hand out their own guard for the duration.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

/// Shared connection counter. Cheap to clone; one instance per
/// workflow process.
#[derive(Clone, Default)]
pub struct IdleTracker {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    count: AtomicUsize,
    /// Fired whenever `count` changes. The watcher loop uses it to
    /// wake from its idle-grace sleep when a new connection arrives.
    notify: Notify,
}

impl IdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a guard for a single connection. The count returns to its
    /// previous value when the guard is dropped, so callers should
    /// hold it for the lifetime of the connection's serving task.
    pub fn guard(&self) -> ConnectionGuard {
        self.inner.count.fetch_add(1, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
        ConnectionGuard {
            inner: self.inner.clone(),
        }
    }

    fn count(&self) -> usize {
        self.inner.count.load(Ordering::SeqCst)
    }

    async fn wait_change(&self) {
        self.inner.notify.notified().await;
    }
}

pub struct ConnectionGuard {
    inner: Arc<Inner>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.inner.count.fetch_sub(1, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

/// Resolve once the connection count has been continuously zero for
/// `idle_timeout`. Returns immediately on the next zero-streak after a
/// connection drops; a new connection arriving during the grace window
/// resets the timer.
pub async fn wait_until_idle(tracker: IdleTracker, idle_timeout: Duration) {
    loop {
        // Block until count is zero.
        while tracker.count() > 0 {
            tracker.wait_change().await;
        }
        // Race the grace timer against the next count change. If a
        // new connection arrives we loop and re-check; if the timer
        // wins, the workflow has been idle long enough to exit.
        tokio::select! {
            _ = tokio::time::sleep(idle_timeout) => {
                if tracker.count() == 0 {
                    return;
                }
            }
            _ = tracker.wait_change() => {
                // count flipped — recheck
            }
        }
    }
}
