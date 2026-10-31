//! CancelCheck — the cancellation probe engine loops poll between pages.
//!
//! This file is fork-only code (NOT upstream).
//!
//! A loop that does CPU work between I/O calls (all-pages extract, search,
//! image reduction, validation, conversion) calls [`CancelCheck::check`]
//! at the top of each iteration, so a cancel stops it within one page,
//! one image or one rule batch instead of at the next I/O boundary.
//!
//!   native — wraps the lane's kill flag and the job's cancel flag.
//!   wasm32 — reads the job's cancel word through the `host_cancelled`
//!            import (a SharedArrayBuffer word the main thread stores to).
//!   none   — never cancelled: every caller outside a lane, and tests.

use crate::error::{Error, Result};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

/// Cheap-to-clone cancellation probe.
#[derive(Clone, Default, Debug)]
pub struct CancelCheck {
    #[cfg(not(target_arch = "wasm32"))]
    flags: Option<(Arc<AtomicBool>, Arc<AtomicBool>)>,
    #[cfg(target_arch = "wasm32")]
    host: bool,
}

impl CancelCheck {
    /// A check that is never cancelled.
    pub fn none() -> Self {
        Self::default()
    }

    /// A check wired to a lane's kill flag and one job's cancel flag.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_flags(lane: Arc<AtomicBool>, job: Arc<AtomicBool>) -> Self {
        Self { flags: Some((lane, job)) }
    }

    /// A check that reads the current job's cancel word from the host.
    #[cfg(target_arch = "wasm32")]
    pub fn from_host() -> Self {
        Self { host: true }
    }

    /// True once the lane or the job has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            match &self.flags {
                Some((lane, job)) => {
                    lane.load(Ordering::Relaxed) || job.load(Ordering::Relaxed)
                }
                None => false,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.host && crate::host::wasm::host_cancelled() != 0
        }
    }

    /// `Err` with the cancelled I/O error once cancelled.
    ///
    /// The error is the one the readers and writers return, so a loop
    /// check and an I/O check are indistinguishable to the caller.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Io(std::io::Error::other("operation cancelled")))
        } else {
            Ok(())
        }
    }

    /// Runs `f` on each item, checking before every one: a cancel stops
    /// the loop within one item with the cancelled error. A loop whose
    /// body must `break`, `continue` or return early keeps a plain
    /// [`check`](Self::check) instead.
    pub fn each<T>(
        &self,
        items: impl IntoIterator<Item = T>,
        mut f: impl FnMut(T) -> Result<()>,
    ) -> Result<()> {
        for item in items {
            self.check()?;
            f(item)?;
        }
        Ok(())
    }
}

/// Iterations of an inner loop between two polls of a [`CancelCheck`]: a
/// poll is a relaxed atomic load, cheap, but not free in a loop whose body is
/// a few instructions.
pub(crate) const STRIDE: u32 = 4096;

/// Polls a [`CancelCheck`] once per [`STRIDE`] iterations of an inner loop.
pub(crate) struct Stride<'a> {
    cancel: &'a CancelCheck,
    count: u32,
}

impl<'a> Stride<'a> {
    pub(crate) fn new(cancel: &'a CancelCheck) -> Self {
        Self { cancel, count: 0 }
    }

    /// Counts one iteration; every [`STRIDE`]th one checks for a cancel.
    pub(crate) fn tick(&mut self) -> Result<()> {
        self.count = self.count.wrapping_add(1);
        if self.count % STRIDE == 0 {
            self.cancel.check()
        } else {
            Ok(())
        }
    }
}
