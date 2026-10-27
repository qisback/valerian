// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Valerian core: deep-sky image maths with no GPU dependency.
//!
//! The CPU implementation is the reference that any GPU kernel must match.
//! Shared image types come from `helia-core` (see issue #4 for what
//! is to move into a common workspace).
#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are bounded well below i64::MAX and signed offsets are needed at borders"
)]

pub mod about;
pub mod background;
pub mod calibrate;
pub mod cancel;
pub mod catalogue;
pub mod combine;
pub mod compute;
pub mod debayer;
pub mod deconvolve;
pub mod drizzle;
pub mod error;
pub mod fits;
pub mod mosaic;
pub mod photocal;
pub mod preview;
pub mod pyramid;
pub mod register;
pub mod solve;
pub mod stack;
pub mod stack_stream;
pub mod starless;
pub mod stars;
pub mod stats;
pub mod streaming;
pub mod stretch;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
pub mod transform;
pub mod warp;
pub mod wcs;

pub use cancel::CancelToken;
pub use error::{Error, Result};

pub use helia_core::{BayerPattern, Frame};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helia_types_are_available() {
        let f = Frame::zeros(4, 3);
        assert_eq!((f.width, f.height), (4, 3));
        assert_eq!(BayerPattern::Rggb.channel_at(0, 0), 0);
    }
}
