//! Supervision for the background upkeep jobs (review finding 12).
//!
//! The jobs [`supervise`] runs are infinite loops: a task that terminates has
//! panicked, been cancelled, or hit a bug. Silently dropping its
//! [`tokio::task::JoinHandle`] would hide that forever, so the supervisor
//! keeps the handle, logs `error!` when the task terminates, and restarts the
//! job with exponential backoff — see [`supervise`] for the policy.

use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

/// First restart delay; doubles after every failed run up to [`BACKOFF_CAP`].
const BACKOFF_START: Duration = Duration::from_secs(1);

/// Upper bound of the restart backoff.
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// A run at least this long counts as healthy: the backoff resets, so a job
/// that dies once an hour is restarted promptly, while one that dies in a
/// tight loop backs off towards the cap.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Runs `spawn` in a loop and never gives up on the job. When the spawned
/// task terminates for any reason (panic, cancellation, or an early return —
/// all three are bugs in an infinite-loop job), the supervisor logs `error!`
/// and restarts it after the current backoff (1 s doubling to 60 s, reset
/// after a healthy run of at least [`HEALTHY_RUN`]). The gateway keeps
/// serving while the job is down.
///
/// The returned handle is the supervisor task itself, which runs for the
/// lifetime of the process; the individual job handles are awaited inside it.
/// Failures: none — a failing job is logged and retried forever.
pub fn supervise(
    name: &'static str,
    spawn: impl Fn() -> JoinHandle<()> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = BACKOFF_START;
        loop {
            let started = Instant::now();
            match spawn().await {
                Ok(()) => tracing::error!(job = name, "background job exited; restarting"),
                Err(e) => tracing::error!(
                    job = name,
                    error = %e,
                    "background job panicked or was cancelled; restarting"
                ),
            }
            if started.elapsed() >= HEALTHY_RUN {
                backoff = BACKOFF_START;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_CAP);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// The supervisor must restart a job whose task panics (the panic output
    /// on stderr from the first run is expected: the task dies the way any
    /// panicking task does, and the supervisor observes it through the
    /// `JoinHandle`).
    #[tokio::test]
    async fn restarts_a_task_that_panics_once() {
        let runs = Arc::new(AtomicUsize::new(0));
        let restarted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _supervisor = supervise("panics-once", {
            let runs = Arc::clone(&runs);
            let restarted = Arc::clone(&restarted);
            move || {
                let runs = Arc::clone(&runs);
                let restarted = Arc::clone(&restarted);
                tokio::spawn(async move {
                    if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("first run dies");
                    }
                    restarted.store(true, Ordering::SeqCst);
                })
            }
        });

        // The second run must start within the first backoff (1 s) plus
        // scheduling slack.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !restarted.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the supervisor did not restart the panicked task"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(runs.load(Ordering::SeqCst) >= 2, "the task ran again");
    }
}
