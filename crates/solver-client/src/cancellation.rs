use std::sync::{
    Arc, Condvar, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// Cloneable cooperative cancellation signal for a running sidecar invocation.
///
/// Cancellation is idempotent. The client observes it promptly, terminates the
/// associated child process, drains its pipes, and reports `Cancelled` rather
/// than a solver failure.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    wait_lock: Mutex<()>,
    wake: Condvar,
}

impl CancellationToken {
    /// Create a token in the non-cancelled state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Signal cancellation and wake a client waiting on this token.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
        self.state.wake.notify_all();
    }

    /// Return whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let guard = self
            .state
            .wait_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _guard = self
            .state
            .wake
            .wait_timeout_while(guard, timeout, |()| !self.is_cancelled())
            .unwrap_or_else(PoisonError::into_inner);
        self.is_cancelled()
    }
}
