//! Shared spin / yield / sleep escalation for blocking ring pushes.
//!
//! Callers spin in a tight loop on a ring's `push`; on `Err` they call
//! [`step`] with the spin counter, which picks the right back-off strategy
//! for the current iteration count. Centralized so the escalation curve
//! lives in one place.

/// Advance the back-off given the current spin count, then return the new
/// count. The first 64 iterations are pure `spin_loop`, the next 192
/// `yield_now`, after that a 10 µs sleep.
#[inline]
pub fn step(spins: u32) -> u32 {
    if spins < 64 {
        std::hint::spin_loop();
    } else if spins < 256 {
        std::thread::yield_now();
    } else {
        std::thread::sleep(std::time::Duration::from_micros(10));
    }
    spins.saturating_add(1)
}
