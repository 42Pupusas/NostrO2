//! Reconnection policy.
//!
//! [`ReconnectConfig`] is the user's policy. [`ReconnectSchedule`] is the
//! driver's live view of it: an iterator that yields the next delay and stops
//! when the retry budget runs out. The driver therefore never counts attempts
//! itself, and the "should I retry?" decision lives in exactly one place.

/// Configuration for automatic reconnection with exponential backoff.
///
/// A delay of zero disables reconnection. Delays keep sub-second precision:
/// `max_delay` is compared against [`std::time::Duration::ZERO`], not against
/// a whole number of seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconnectConfig {
    /// Number of consecutive failures to tolerate. Zero means unlimited.
    pub max_retries: u32,
    /// Delay before the first reconnection attempt.
    pub initial_delay: std::time::Duration,
    /// Ceiling on the delay between attempts. Zero disables reconnection.
    pub max_delay: std::time::Duration,
    /// Growth factor applied to the delay after each failure.
    pub backoff_multiplier: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: 0,
            initial_delay: std::time::Duration::from_secs(1),
            max_delay: std::time::Duration::from_secs(60),
            backoff_multiplier: 2.0,
        }
    }
}

impl ReconnectConfig {
    /// A policy that never reconnects.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            max_retries: 0,
            initial_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
            backoff_multiplier: 0.0,
        }
    }

    /// A policy that retries forever at a fixed `delay`.
    #[must_use]
    pub const fn fixed(delay: std::time::Duration) -> Self {
        Self {
            max_retries: 0,
            initial_delay: delay,
            max_delay: delay,
            backoff_multiplier: 1.0,
        }
    }

    /// Whether the driver reconnects at all.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        !self.max_delay.is_zero()
    }

    /// The delay after `attempt` consecutive failures, counting from zero.
    ///
    /// The result is clamped to `max_delay` and is never negative, whatever
    /// the multiplier is.
    #[must_use]
    pub fn next_delay(&self, attempt: u32) -> std::time::Duration {
        if !self.is_enabled() {
            return std::time::Duration::ZERO;
        }
        let ceiling = self.max_delay.as_secs_f64();
        let growth = self.backoff_multiplier.powf(f64::from(attempt));
        let delay = self.initial_delay.as_secs_f64() * growth;
        if !delay.is_finite() || delay >= ceiling {
            return self.max_delay;
        }
        std::time::Duration::from_secs_f64(delay.max(0.0))
    }

    /// Starts a fresh schedule from this policy.
    #[must_use]
    pub fn schedule(&self) -> ReconnectSchedule {
        ReconnectSchedule {
            config: self.clone(),
            attempt: 0,
        }
    }
}

/// The retry budget of one connection, consumed one delay at a time.
///
/// `next()` yields the delay to wait before the next attempt and `None` once
/// the policy gives up. [`Self::succeeded`] returns the budget to full, so a
/// connection that lives for a while does not inherit the backoff of the one
/// before it.
#[derive(Debug, Clone)]
pub struct ReconnectSchedule {
    config: ReconnectConfig,
    attempt: u32,
}

impl ReconnectSchedule {
    /// The number of failures since the last success.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Resets the budget after a successful connection.
    pub const fn succeeded(&mut self) {
        self.attempt = 0;
    }

    /// Whether another attempt is allowed.
    #[must_use]
    pub const fn has_budget(&self) -> bool {
        self.config.is_enabled()
            && (self.config.max_retries == 0 || self.attempt < self.config.max_retries)
    }

    /// The policy this schedule runs.
    #[must_use]
    pub const fn config(&self) -> &ReconnectConfig {
        &self.config
    }
}

impl Iterator for ReconnectSchedule {
    type Item = std::time::Duration;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.has_budget() {
            return None;
        }
        let delay = self.config.next_delay(self.attempt);
        self.attempt += 1;
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Policy;

    impl Policy {
        fn doubling() -> ReconnectConfig {
            ReconnectConfig {
                max_retries: 0,
                initial_delay: Duration::from_secs(1),
                max_delay: Duration::from_secs(8),
                backoff_multiplier: 2.0,
            }
        }

        fn sub_second() -> ReconnectConfig {
            ReconnectConfig {
                max_retries: 0,
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(40),
                backoff_multiplier: 2.0,
            }
        }
    }

    #[test]
    fn the_default_policy_retries_forever() {
        let config = ReconnectConfig::default();
        assert!(config.is_enabled());
        assert_eq!(config.max_retries, 0);
        assert!(config.schedule().has_budget());
    }

    #[test]
    fn a_disabled_policy_yields_nothing() {
        let config = ReconnectConfig::disabled();
        assert!(!config.is_enabled());
        assert_eq!(config.next_delay(0), Duration::ZERO);
        assert_eq!(config.schedule().next(), None);
    }

    #[test]
    fn delays_double_until_they_hit_the_ceiling() {
        let config = Policy::doubling();
        let delays: Vec<_> = config.schedule().take(5).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }

    // The old policy tested `max_delay.as_secs() > 0`, so a millisecond
    // ceiling read as "reconnection disabled" and a test that wanted a fast
    // retry silently got no retry at all.
    #[test]
    fn a_sub_second_ceiling_still_enables_reconnection() {
        let config = Policy::sub_second();
        assert!(config.is_enabled());
        let delays: Vec<_> = config.schedule().take(3).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(40),
            ]
        );
    }

    #[test]
    fn a_fixed_policy_repeats_one_delay() {
        let config = ReconnectConfig::fixed(Duration::from_millis(250));
        let delays: Vec<_> = config.schedule().take(3).collect();
        assert_eq!(delays, vec![Duration::from_millis(250); 3]);
    }

    #[test]
    fn a_retry_limit_ends_the_schedule() {
        let config = ReconnectConfig {
            max_retries: 3,
            ..Policy::doubling()
        };
        assert_eq!(config.schedule().count(), 3);
    }

    #[test]
    fn success_restores_the_budget_and_the_delay() {
        let config = ReconnectConfig {
            max_retries: 2,
            ..Policy::doubling()
        };
        let mut schedule = config.schedule();
        assert_eq!(schedule.next(), Some(Duration::from_secs(1)));
        assert_eq!(schedule.next(), Some(Duration::from_secs(2)));
        assert_eq!(schedule.next(), None);

        schedule.succeeded();
        assert_eq!(schedule.attempt(), 0);
        assert!(schedule.has_budget());
        assert_eq!(schedule.next(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn the_attempt_count_tracks_consecutive_failures() {
        let mut schedule = Policy::doubling().schedule();
        assert_eq!(schedule.attempt(), 0);
        schedule.next();
        schedule.next();
        assert_eq!(schedule.attempt(), 2);
    }

    #[test]
    fn a_huge_multiplier_saturates_instead_of_panicking() {
        let config = ReconnectConfig {
            backoff_multiplier: f64::MAX,
            ..Policy::doubling()
        };
        assert_eq!(config.next_delay(64), config.max_delay);
    }

    #[test]
    fn a_negative_multiplier_never_yields_a_negative_delay() {
        let config = ReconnectConfig {
            backoff_multiplier: -2.0,
            ..Policy::doubling()
        };
        for attempt in 0..8 {
            let delay = config.next_delay(attempt);
            assert!(delay <= config.max_delay);
        }
    }

    #[test]
    fn a_zero_multiplier_collapses_to_the_initial_delay_then_zero() {
        let config = ReconnectConfig {
            backoff_multiplier: 0.0,
            ..Policy::doubling()
        };
        assert_eq!(config.next_delay(0), Duration::from_secs(1));
        assert_eq!(config.next_delay(1), Duration::ZERO);
    }

    #[test]
    fn a_schedule_carries_its_policy() {
        let config = Policy::doubling();
        assert_eq!(config.schedule().config(), &config);
    }
}
