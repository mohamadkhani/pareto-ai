//! Per-(credential, offering) circuit breaker (M5).
//!
//! **Coherence with the M3 cooldown** (see also [`crate::chat`]): the two
//! mechanisms sit at different layers and both stay in force.
//!
//! * **M3 cooldown** (`provider_credentials.cooldown_until`): credential-wide,
//!   written after an upstream 401/403/402/429 (auth, quota, rate limit) for
//!   the `Retry-After` window (default 60s). [`crate::chat`] demotes cooled
//!   credentials in the candidate order.
//! * **M5 breaker**: scoped to one (credential, offering) pair, counts every
//!   classified attempt failure that reflects the offering's service —
//!   transient 5xx, timeouts, auth, quota, and rate limits included — but
//!   **not** `invalid_request` (the caller's broken request) or `internal`
//!   (our own configuration); those never exclude a healthy offering (review
//!   M1, [`crate::upstream::FailureClass::counts_toward_breaker`]). It opens
//!   after [`BreakerConfig::threshold`] consecutive failures with no success
//!   in between. While open the pair is **excluded** from routing (visible in
//!   `decision_json` as `skipped_breaker_open`) for
//!   [`BreakerConfig::cooldown`], then half-opens: the window does not admit a
//!   single probe — any number of concurrent requests may pass, the first
//!   success closes the pair again, and a failed probe re-opens it
//!   immediately (review m3).
//!
//! A rate-limited request therefore sets both: the credential cooldown (M3)
//! at once, and one failure count (M5) toward exclusion after N such events.
//!
//! State is in-memory (like [`crate::api_keys::KeyRateLimiter`] and the login
//! limiter): a restart forgets trips, while credential cooldowns persist in
//! the database. Time is injected (`now`) so the state machine is unit-tested
//! without sleeping.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Consecutive failures before a pair is excluded (config default 5).
pub const DEFAULT_THRESHOLD: u32 = 5;
/// How long an open breaker excludes a pair (config default 60s).
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);

/// Breaker shape: the consecutive-failure threshold and the cooldown window.
#[derive(Debug, Clone, Copy)]
pub struct BreakerConfig {
    pub threshold: u32,
    pub cooldown: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            cooldown: DEFAULT_COOLDOWN,
        }
    }
}

/// The breaker state of one (credential, offering) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerStatus {
    /// Counting consecutive failures.
    Closed,
    /// Excluded until `until`.
    Open { until: Instant },
    /// The window elapsed: a probe is allowed; its failure re-opens.
    HalfOpen,
}

#[derive(Debug, Default, Clone)]
struct PairState {
    consecutive: u32,
    open_until: Option<Instant>,
    /// Total trips, for the "recently tripped" recovery signal.
    trips: u32,
    last_trip_at: Option<Instant>,
}

/// The shared breaker registry: `(credential_id, offering_id)` → state.
#[derive(Debug)]
pub struct BreakerRegistry {
    config: BreakerConfig,
    states: Mutex<HashMap<(String, String), PairState>>,
}

impl BreakerRegistry {
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            states: Mutex::new(HashMap::new()),
        }
    }

    /// The pair's status at `now` (an elapsed window flips to half-open).
    pub fn status(&self, credential_id: &str, offering_id: &str, now: Instant) -> BreakerStatus {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = states.get(&(credential_id.to_string(), offering_id.to_string())) else {
            return BreakerStatus::Closed;
        };
        match state.open_until {
            Some(until) if now < until => BreakerStatus::Open { until },
            Some(_) => BreakerStatus::HalfOpen,
            None => BreakerStatus::Closed,
        }
    }

    /// Whether the pair is excluded at `now` (open; half-open probes pass).
    pub fn is_open(&self, credential_id: &str, offering_id: &str, now: Instant) -> bool {
        matches!(
            self.status(credential_id, offering_id, now),
            BreakerStatus::Open { .. }
        )
    }

    /// Whether the pair tripped recently but is not open: a recovery signal
    /// the router penalizes until the pair proves itself again.
    pub fn is_recovering(&self, credential_id: &str, offering_id: &str, now: Instant) -> bool {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = states.get(&(credential_id.to_string(), offering_id.to_string())) else {
            return false;
        };
        if state.open_until.is_some_and(|until| now < until) {
            return false;
        }
        state
            .last_trip_at
            .is_some_and(|at| now.duration_since(at) < self.config.cooldown * 2)
    }

    /// Records one failed attempt and returns the new status. A failure in
    /// half-open re-opens the pair immediately; a closed pair opens at the
    /// threshold. Callers gate on
    /// [`crate::upstream::FailureClass::counts_toward_breaker`] (review M1):
    /// caller-fault and internal failures never reach this method. Failure
    /// mode: none — worst case a stale lock is taken over.
    pub fn record_failure(
        &self,
        credential_id: &str,
        offering_id: &str,
        now: Instant,
    ) -> BreakerStatus {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states
            .entry((credential_id.to_string(), offering_id.to_string()))
            .or_default();
        state.consecutive = state.consecutive.saturating_add(1);

        let was_open = state.open_until.is_some_and(|until| now < until);
        let half_open_probe_failed = state.open_until.is_some_and(|until| now >= until);
        if was_open {
            // Raced an exclusion: keep the pair out for another window.
            state.open_until = Some(now + self.config.cooldown);
            return BreakerStatus::Open {
                until: now + self.config.cooldown,
            };
        }
        if half_open_probe_failed || state.consecutive >= self.config.threshold {
            state.open_until = Some(now + self.config.cooldown);
            state.trips = state.trips.saturating_add(1);
            state.last_trip_at = Some(now);
            return BreakerStatus::Open {
                until: now + self.config.cooldown,
            };
        }
        BreakerStatus::Closed
    }

    /// Records a completed attempt: the streak resets and a half-open probe
    /// that succeeded closes the pair again.
    pub fn record_success(&self, credential_id: &str, offering_id: &str) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states
            .entry((credential_id.to_string(), offering_id.to_string()))
            .or_default();
        state.consecutive = 0;
        state.open_until = None;
    }

    /// Trips recorded for a pair (diagnostics and tests).
    pub fn trips(&self, credential_id: &str, offering_id: &str) -> u32 {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(credential_id.to_string(), offering_id.to_string()))
            .map(|state| state.trips)
            .unwrap_or(0)
    }

    /// The pair's current consecutive-failure streak — the number the
    /// breaker's explanation reports (review M2: `trips()` counts *opens*,
    /// which is a different number).
    pub fn consecutive(&self, credential_id: &str, offering_id: &str) -> u32 {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(credential_id.to_string(), offering_id.to_string()))
            .map(|state| state.consecutive)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(threshold: u32) -> BreakerRegistry {
        BreakerRegistry::new(BreakerConfig {
            threshold,
            cooldown: Duration::from_secs(10),
        })
    }

    #[test]
    fn opens_after_the_configured_consecutive_failures() {
        let breaker = registry(3);
        let now = Instant::now();
        for _ in 0..2 {
            assert_eq!(
                breaker.record_failure("cred", "off", now),
                BreakerStatus::Closed
            );
        }
        let opened = breaker.record_failure("cred", "off", now);
        assert!(matches!(opened, BreakerStatus::Open { .. }));
        assert!(breaker.is_open("cred", "off", now));
        assert_eq!(breaker.trips("cred", "off"), 1);
    }

    #[test]
    fn a_success_resets_the_streak() {
        let breaker = registry(3);
        let now = Instant::now();
        breaker.record_failure("cred", "off", now);
        breaker.record_failure("cred", "off", now);
        breaker.record_success("cred", "off");
        assert_eq!(
            breaker.record_failure("cred", "off", now),
            BreakerStatus::Closed
        );
        assert_eq!(
            breaker.record_failure("cred", "off", now),
            BreakerStatus::Closed
        );
        assert!(!breaker.is_open("cred", "off", now));
    }

    #[test]
    fn consecutive_reports_the_streak_not_the_trip_count() {
        // Review M2: the explanation must say "N consecutive failures", not
        // the number of times the breaker opened.
        let breaker = registry(3);
        let now = Instant::now();
        assert_eq!(breaker.consecutive("cred", "off"), 0);
        breaker.record_failure("cred", "off", now);
        breaker.record_failure("cred", "off", now);
        assert_eq!(breaker.consecutive("cred", "off"), 2);
        assert_eq!(breaker.trips("cred", "off"), 0, "not opened yet");
        breaker.record_success("cred", "off");
        assert_eq!(
            breaker.consecutive("cred", "off"),
            0,
            "a success resets the streak"
        );
    }

    #[test]
    fn an_open_pair_excludes_until_the_window_then_half_opens() {
        let breaker = registry(1);
        let now = Instant::now();
        breaker.record_failure("cred", "off", now);
        assert!(breaker.is_open("cred", "off", now));
        assert!(breaker.is_open("cred", "off", now + Duration::from_secs(9)));

        let later = now + Duration::from_secs(11);
        assert!(!breaker.is_open("cred", "off", later), "recovery window");
        assert_eq!(
            breaker.status("cred", "off", later),
            BreakerStatus::HalfOpen
        );
    }

    #[test]
    fn a_failed_probe_reopens_immediately_and_a_successful_one_closes() {
        let breaker = registry(2);
        let now = Instant::now();
        breaker.record_failure("cred", "off", now);
        breaker.record_failure("cred", "off", now);
        let later = now + Duration::from_secs(11);

        assert!(matches!(
            breaker.record_failure("cred", "off", later),
            BreakerStatus::Open { .. }
        ));
        assert!(breaker.is_open("cred", "off", later), "re-opened");
        assert_eq!(breaker.trips("cred", "off"), 2);

        let even_later = later + Duration::from_secs(11);
        breaker.record_success("cred", "off");
        assert_eq!(
            breaker.status("cred", "off", even_later),
            BreakerStatus::Closed
        );
        assert_eq!(
            breaker.record_failure("cred", "off", even_later),
            BreakerStatus::Closed,
            "the streak restarted at zero"
        );
    }

    #[test]
    fn the_recovery_signal_lingers_after_a_trip() {
        let breaker = registry(1);
        let now = Instant::now();
        assert!(!breaker.is_recovering("cred", "off", now));
        breaker.record_failure("cred", "off", now);
        assert!(
            !breaker.is_recovering("cred", "off", now),
            "open pairs are excluded, not merely penalized"
        );
        let later = now + Duration::from_secs(11);
        assert!(breaker.is_recovering("cred", "off", later));
        assert!(
            !breaker.is_recovering("cred", "off", later + Duration::from_secs(25)),
            "the signal fades"
        );
    }

    #[test]
    fn breakers_are_scoped_per_credential_and_offering() {
        let breaker = registry(1);
        let now = Instant::now();
        breaker.record_failure("cred-1", "off-1", now);
        assert!(breaker.is_open("cred-1", "off-1", now));
        assert!(!breaker.is_open("cred-2", "off-1", now), "other credential");
        assert!(!breaker.is_open("cred-1", "off-2", now), "other offering");
    }
}
