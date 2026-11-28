//! Per-provider circuit breaker and rate-limit tracker for resilient market-data fallback chains.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use crate::clock::AppClock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
struct ProviderSlot {
    consecutive_failures: u32,
    opened_until: Option<DateTime<Utc>>,
    last_request_at: Option<DateTime<Utc>>,
}

impl Default for ProviderSlot {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            opened_until: None,
            last_request_at: None,
        }
    }
}

/// Thread-safe circuit breaker registry keyed by provider slug (`yahoo`, `boerse_frankfurt`,
/// `eodhd`, `marketstack`, etc.).
pub struct ProviderCircuitBreaker {
    clock: AppClock,
    failure_threshold: u32,
    open_duration: Duration,
    min_interval: Duration,
    slots: Mutex<HashMap<String, ProviderSlot>>,
}

impl ProviderCircuitBreaker {
    pub fn new(clock: AppClock) -> Self {
        Self {
            clock,
            failure_threshold: 3,
            open_duration: Duration::minutes(5),
            min_interval: Duration::milliseconds(200),
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub fn state(&self, provider: &str) -> BreakerState {
        let now = self.clock.now();
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = slots.get(provider) else {
            return BreakerState::Closed;
        };
        match slot.opened_until {
            Some(until) if now < until => BreakerState::Open,
            Some(_) => BreakerState::HalfOpen,
            None => BreakerState::Closed,
        }
    }

    /// Returns `true` when the provider is allowed to handle a request now.
    pub fn allow_request(&self, provider: &str) -> bool {
        let now = self.clock.now();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(provider.to_owned()).or_default();
        if let Some(until) = slot.opened_until
            && now < until
        {
            return false;
        }
        if let Some(last) = slot.last_request_at
            && now - last < self.min_interval
        {
            return false;
        }
        slot.last_request_at = Some(now);
        true
    }

    pub fn record_success(&self, provider: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(provider.to_owned()).or_default();
        slot.consecutive_failures = 0;
        slot.opened_until = None;
    }

    pub fn record_failure(&self, provider: &str) {
        let now = self.clock.now();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(provider.to_owned()).or_default();
        slot.consecutive_failures = slot.consecutive_failures.saturating_add(1);
        if slot.consecutive_failures >= self.failure_threshold {
            slot.opened_until = Some(now + self.open_duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use chrono::TimeZone;
    use chrono_tz::Tz;
    use crate::clock::FixedClock;
    use super::*;

    #[test]
    fn opens_after_threshold_and_recovers_after_cooldown() {
        let t0 = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let fixed = Arc::new(FixedClock::new(t0));
        let cb = ProviderCircuitBreaker::new(AppClock::new(fixed.clone(), Tz::UTC));

        assert_eq!(cb.state("eodhd"), BreakerState::Closed);
        cb.record_failure("eodhd");
        cb.record_failure("eodhd");
        assert_eq!(cb.state("eodhd"), BreakerState::Closed);
        cb.record_failure("eodhd");
        assert_eq!(cb.state("eodhd"), BreakerState::Open);
        assert!(!cb.allow_request("eodhd"));

        fixed.advance(std::time::Duration::from_secs(360));
        assert_eq!(cb.state("eodhd"), BreakerState::HalfOpen);
        assert!(cb.allow_request("eodhd"));
        cb.record_success("eodhd");
        assert_eq!(cb.state("eodhd"), BreakerState::Closed);
    }
}
