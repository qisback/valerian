// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The error type shared by the processing modules.

use crate::fits::FitsError;

/// Processing failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A FITS file could not be read or written.
    #[error(transparent)]
    Fits(#[from] FitsError),
    /// Inputs are missing, empty or do not fit together.
    #[error("{0}")]
    Invalid(String),
    /// Registration could not find a consistent transform.
    #[error("registration failed: {0}")]
    NoMatch(String),
    /// The run was cancelled through its [`crate::CancelToken`].
    #[error("cancelled")]
    Cancelled,
}

/// Result alias for processing functions.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// An [`Error::Invalid`] from anything printable.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}
