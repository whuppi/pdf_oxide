//! This thread's CPU clock, for the proofs and guards that bound how long an
//! operation takes. The wall clock counts every other test the runner has in
//! flight, so a bound on it fails when the machine is busy; a thread's own CPU
//! time does not move with the load. The integration tests in `tests/` reach it
//! through the `test-support` feature.

use std::time::Duration;

/// The CPU time this thread has spent so far.
pub fn thread_cpu() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-pointer for the one call.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// [`thread_cpu`] in seconds.
pub fn thread_cpu_secs() -> f64 {
    thread_cpu().as_secs_f64()
}

/// A moment on this thread's CPU clock, read back as a stopwatch.
pub struct CpuInstant(Duration);

impl CpuInstant {
    /// This moment on this thread's CPU clock.
    pub fn now() -> Self {
        CpuInstant(thread_cpu())
    }

    /// The CPU time this thread has spent since `self`.
    pub fn elapsed(&self) -> Duration {
        thread_cpu().saturating_sub(self.0)
    }
}
