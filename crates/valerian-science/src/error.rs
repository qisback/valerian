// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The error type of the science outputs.

/// Failures of the science outputs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The plot could not be drawn.
    #[error("plot: {0}")]
    Plot(String),
    /// A table or manifest could not be read.
    #[error("{0}")]
    Parse(String),
    /// The grader model could not be loaded or run.
    #[error("{0}")]
    Model(String),
    /// Nothing to plot or report.
    #[error("{0}")]
    Empty(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// An I/O error that names its file.
    #[must_use]
    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    /// A plotting error from anything printable.
    #[must_use]
    pub fn plot(e: impl std::fmt::Display) -> Self {
        Self::Plot(e.to_string())
    }
}
