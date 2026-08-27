//! Liveness probing for one connection.
//!
//! TCP does not report a peer that stops answering. A NAT drops the
//! mapping, a laptop suspends, a middlebox times the flow out: the socket
//! stays open, `read` blocks forever, and nothing ever arrives. A driver
//! that trusts the socket therefore waits for events that can never come,
//! and never reconnects, because it believes it is still connected.
//!
//! [`Heartbeat`] turns that silence into a decision. It sends a WebSocket
//! ping after an idle period, and declares the connection dead when the
//! relay answers neither the ping nor anything else within a deadline.
//!
//! Any inbound traffic counts as proof of life, so a busy connection never
//! pays for a probe.

/// When to probe a quiet connection, and when to give up on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatConfig {
    /// Silence tolerated before a ping goes out.
    pub idle_timeout: std::time::Duration,
    /// Silence tolerated after that ping before the connection is dead.
    pub reply_timeout: std::time::Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            idle_timeout: std::time::Duration::from_secs(45),
            reply_timeout: std::time::Duration::from_secs(20),
        }
    }
}

impl HeartbeatConfig {
    /// A policy that never probes.
    ///
    /// The driver then cannot tell a quiet relay from a dead one, so a
    /// broken connection stalls until the operating system gives up on the
    /// socket, which can take hours.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            idle_timeout: std::time::Duration::ZERO,
            reply_timeout: std::time::Duration::ZERO,
        }
    }

    /// Whether the driver probes at all.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        !self.idle_timeout.is_zero()
    }
}

/// What the driver must do next about liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The connection is healthy. Keep reading.
    Healthy,
    /// The connection has been quiet. Send a ping.
    Probe,
    /// The relay answered nothing after a probe. Drop the connection.
    Dead,
}

/// Tracks silence on one connection.
///
/// The driver reports every inbound frame with [`Self::saw_traffic`], then
/// asks [`Self::assess`] once per loop what to do.
#[derive(Debug)]
pub struct Heartbeat {
    config: HeartbeatConfig,
    last_seen: std::time::Instant,
    probed_at: Option<std::time::Instant>,
}

impl Heartbeat {
    /// Starts tracking a connection that is live right now.
    #[must_use]
    pub fn new(config: HeartbeatConfig) -> Self {
        Self {
            config,
            last_seen: std::time::Instant::now(),
            probed_at: None,
        }
    }

    /// Records proof that the relay is alive.
    ///
    /// Any inbound frame counts, including a pong and including traffic that
    /// arrives while a probe is outstanding.
    pub fn saw_traffic(&mut self) {
        self.last_seen = std::time::Instant::now();
        self.probed_at = None;
    }

    /// Records that a probe went out, so its reply can be timed.
    pub fn probed(&mut self) {
        self.probed_at = Some(std::time::Instant::now());
    }

    /// The policy this heartbeat applies.
    #[must_use]
    pub const fn config(&self) -> &HeartbeatConfig {
        &self.config
    }

    /// What the driver must do, given the time now.
    #[must_use]
    pub fn assess(&self) -> Liveness {
        self.assess_at(std::time::Instant::now())
    }

    /// [`Self::assess`] against an explicit instant, so tests need no sleep.
    #[must_use]
    pub fn assess_at(&self, now: std::time::Instant) -> Liveness {
        if !self.config.is_enabled() {
            return Liveness::Healthy;
        }
        if let Some(probed_at) = self.probed_at {
            if now.duration_since(probed_at) >= self.config.reply_timeout {
                return Liveness::Dead;
            }
            return Liveness::Healthy;
        }
        if now.duration_since(self.last_seen) >= self.config.idle_timeout {
            return Liveness::Probe;
        }
        Liveness::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a heartbeat whose clock the test controls.
    struct Clock;

    impl Clock {
        const IDLE: std::time::Duration = std::time::Duration::from_secs(45);
        const REPLY: std::time::Duration = std::time::Duration::from_secs(20);

        fn heartbeat() -> Heartbeat {
            Heartbeat::new(HeartbeatConfig {
                idle_timeout: Self::IDLE,
                reply_timeout: Self::REPLY,
            })
        }

        fn after(heartbeat: &Heartbeat, elapsed: std::time::Duration) -> Liveness {
            heartbeat.assess_at(heartbeat.last_seen + elapsed)
        }
    }

    #[test]
    fn a_fresh_connection_is_healthy() {
        assert_eq!(Clock::heartbeat().assess(), Liveness::Healthy);
    }

    #[test]
    fn a_briefly_quiet_connection_is_still_healthy() {
        let heartbeat = Clock::heartbeat();
        assert_eq!(
            Clock::after(&heartbeat, std::time::Duration::from_secs(10)),
            Liveness::Healthy
        );
    }

    #[test]
    fn a_long_silence_asks_for_a_probe() {
        let heartbeat = Clock::heartbeat();
        assert_eq!(Clock::after(&heartbeat, Clock::IDLE), Liveness::Probe);
    }

    #[test]
    fn traffic_postpones_the_probe() {
        let mut heartbeat = Clock::heartbeat();
        let started = heartbeat.last_seen;
        heartbeat.saw_traffic();
        assert_eq!(
            heartbeat.assess_at(started + Clock::IDLE),
            Liveness::Healthy
        );
    }

    // A relay that answers the probe is alive, and the connection must
    // survive: this is the common case on a quiet subscription.
    #[test]
    fn an_answered_probe_restores_health() {
        let mut heartbeat = Clock::heartbeat();
        heartbeat.probed();
        heartbeat.saw_traffic();
        let started = heartbeat.last_seen;
        assert_eq!(
            heartbeat.assess_at(started + Clock::REPLY),
            Liveness::Healthy
        );
    }

    // The failure this module exists for: the relay answers nothing at all.
    #[test]
    fn an_unanswered_probe_declares_the_connection_dead() {
        let mut heartbeat = Clock::heartbeat();
        heartbeat.probed();
        let probed_at = heartbeat.probed_at.unwrap();
        assert_eq!(
            heartbeat.assess_at(probed_at + Clock::REPLY),
            Liveness::Dead
        );
    }

    #[test]
    fn a_pending_probe_is_not_yet_dead() {
        let mut heartbeat = Clock::heartbeat();
        heartbeat.probed();
        let probed_at = heartbeat.probed_at.unwrap();
        assert_eq!(
            heartbeat.assess_at(probed_at + std::time::Duration::from_secs(5)),
            Liveness::Healthy
        );
    }

    // A probe must go out once, not on every pass of the loop. The driver
    // would otherwise flood a quiet relay with pings.
    #[test]
    fn a_probe_is_not_requested_twice() {
        let mut heartbeat = Clock::heartbeat();
        let started = heartbeat.last_seen;
        assert_eq!(heartbeat.assess_at(started + Clock::IDLE), Liveness::Probe);

        heartbeat.probed();
        let probed_at = heartbeat.probed_at.unwrap();
        assert_eq!(heartbeat.assess_at(probed_at), Liveness::Healthy);
    }

    #[test]
    fn a_disabled_policy_never_probes_and_never_dies() {
        let heartbeat = Heartbeat::new(HeartbeatConfig::disabled());
        let started = heartbeat.last_seen;
        assert!(!heartbeat.config().is_enabled());
        assert_eq!(
            heartbeat.assess_at(started + std::time::Duration::from_secs(86_400)),
            Liveness::Healthy
        );
    }

    #[test]
    fn the_default_policy_probes() {
        assert!(HeartbeatConfig::default().is_enabled());
    }
}
