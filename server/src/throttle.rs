//! Bounded write throttles for bookkeeping columns that need "recently" but
//! not "exactly once per request": `api_keys.last_used_at` and
//! `sessions.last_seen_at`. Both are informational for the admin UI, so one
//! write per subject per interval is enough and removes an autocommit write
//! transaction from every request (docs/research/13-performance.md §2).
//!
//! Time is passed in explicitly, so the cadence is unit-testable without
//! sleeping. The map is bounded with least-recently-written eviction; an
//! evicted subject is simply written again on its next request.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct WriteThrottle {
    interval: Duration,
    max_entries: usize,
    last: Mutex<HashMap<String, Instant>>,
}

impl WriteThrottle {
    /// A throttle that allows one write per subject per `interval`, tracking
    /// at most `max_entries` subjects.
    pub fn new(interval: Duration, max_entries: usize) -> Self {
        Self {
            interval,
            max_entries: max_entries.max(1),
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this request should write the subject's column, recording the
    /// decision. `true` at most once per subject per interval.
    pub fn should_write(&self, subject: &str, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = last.get(subject) {
            if now.saturating_duration_since(*previous) < self.interval {
                return false;
            }
            last.insert(subject.to_string(), now);
            return true;
        }
        if last.len() >= self.max_entries
            && let Some(oldest) = last
                .iter()
                .min_by_key(|(_, stamp)| **stamp)
                .map(|(id, _)| id.clone())
        {
            last.remove(&oldest);
        }
        last.insert(subject.to_string(), now);
        true
    }

    /// Forgets a subject after its write failed, so the next request retries.
    pub fn forget(&self, subject: &str) {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(subject);
    }

    /// Number of tracked subjects; bounded by the constructor's limit.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_one_write_per_interval() {
        let throttle = WriteThrottle::new(Duration::from_secs(60), 10);
        let start = Instant::now();
        assert!(throttle.should_write("key-a", start));
        assert!(!throttle.should_write("key-a", start + Duration::from_secs(59)));
        assert!(throttle.should_write("key-a", start + Duration::from_secs(60)));
        // Subjects are independent.
        assert!(throttle.should_write("key-b", start + Duration::from_secs(61)));
    }

    #[test]
    fn forget_allows_an_immediate_retry() {
        let throttle = WriteThrottle::new(Duration::from_secs(60), 10);
        let start = Instant::now();
        assert!(throttle.should_write("key-a", start));
        throttle.forget("key-a");
        assert!(throttle.should_write("key-a", start));
    }

    #[test]
    fn evicts_the_least_recently_written_subject() {
        let throttle = WriteThrottle::new(Duration::from_secs(60), 2);
        let start = Instant::now();
        assert!(throttle.should_write("old", start));
        assert!(throttle.should_write("new", start + Duration::from_secs(1)));
        assert_eq!(throttle.len(), 2);
        assert!(throttle.should_write("third", start + Duration::from_secs(2)));
        assert_eq!(throttle.len(), 2, "the map stays bounded");
        // The evicted subject is written again; a subject still tracked is not.
        assert!(throttle.should_write("old", start + Duration::from_secs(3)));
        assert!(!throttle.should_write("third", start + Duration::from_secs(4)));
    }
}
