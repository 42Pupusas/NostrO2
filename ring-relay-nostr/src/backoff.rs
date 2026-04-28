//! Shared spin / yield / sleep escalation for blocking ring pushes.
//!
//! Callers spin in a tight loop on a ring's `push`; on `Err` they call
//! [`step`] with the spin counter, which picks the right back-off
//! strategy for the current iteration count. Centralized so the
//! escalation curve lives in one place.
//!
//! The escalation curve is microbench-tuned and not user-configurable
//! — runtime knobs here would expose internal contention shape that
//! nobody outside the crate has the context to tune. The named
//! constants below document the breakpoints in one place.

/// Pure `spin_loop` for the first `SPIN_ITERS` iterations.
pub const SPIN_ITERS: u32 = 64;

/// `yield_now` between `SPIN_ITERS` and `YIELD_ITERS`.
pub const YIELD_ITERS: u32 = 256;

/// Sleep duration once both spinning and yielding are exhausted.
pub const SLEEP_DURATION: std::time::Duration = std::time::Duration::from_micros(10);

/// Advance the back-off given the current spin count, then return the new
/// count.
#[inline]
pub fn step(spins: u32) -> u32 {
    if spins < SPIN_ITERS {
        std::hint::spin_loop();
    } else if spins < YIELD_ITERS {
        std::thread::yield_now();
    } else {
        std::thread::sleep(SLEEP_DURATION);
    }
    spins.saturating_add(1)
}
