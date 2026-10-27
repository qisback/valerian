// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Cooperative cancellation of long runs.
//!
//! A [`CancelToken`] is a flag shared between the code that starts a run and
//! the run itself. Whoever holds a clone calls [`CancelToken::cancel`]; the long
//! loops (per frame, per band, per plane, per trial) call
//! [`CancelToken::check`] between units of work and return
//! [`Error::Cancelled`] once it is set. Work already
//! under way in a unit finishes first, so a cancel takes effect within one
//! frame, band or plane, not at once. A default token is never cancelled, so
//! code that does not ask for cancellation behaves exactly as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result};

/// A shared cancel flag. Clones all see the same flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A new token that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the runs holding this token to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) has been called on any clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Ok` while the run may go on.
    ///
    /// # Errors
    /// [`Error::Cancelled`] once the token is cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_token_is_clear_and_clones_share_the_flag() {
        let t = CancelToken::new();
        let c = t.clone();
        assert!(t.check().is_ok() && !c.is_cancelled());
        c.cancel();
        assert!(t.is_cancelled());
        assert!(matches!(t.check(), Err(Error::Cancelled)));
    }

    #[test]
    fn separate_tokens_are_independent() {
        let (a, b) = (CancelToken::new(), CancelToken::default());
        a.cancel();
        assert!(b.check().is_ok());
    }
}
