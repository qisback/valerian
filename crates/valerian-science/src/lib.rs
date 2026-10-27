// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Valerian science outputs: the graphs and tables an astrophotographer
//! reviews (the SubframeSelector, AberrationInspector and PCC views of
//! PixInsight, the plots of Siril), drawn with pure-Rust plotting.
//!
//! Every output is a PNG with its data beside it as CSV and JSON, and a
//! session gets one self-contained HTML report ([`report`]).
#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and r, g, b are the natural names in plot and image maths"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel and tick coordinates are bounded well below i32::MAX"
)]
#![allow(
    clippy::too_many_lines,
    reason = "plot drawing is a long sequence of series"
)]
#![allow(
    clippy::needless_range_loop,
    reason = "the small dense solvers index several arrays at once"
)]
#![allow(
    clippy::doc_markdown,
    reason = "product names such as PixInsight and SubframeSelector are not code"
)]

pub mod annotate;
pub mod astro_report;
pub mod error;
pub mod fit;
pub mod grader;
pub mod histogram;
pub mod integration;
pub mod integration_report;
pub mod objects;
pub mod optics;
pub mod optics_report;
pub mod pcc;
pub mod photometry;
pub mod plot;
pub mod report;
pub mod residual_map;
pub mod subframe;
pub mod table;
pub mod timeutil;
pub mod zeropoint;

pub use error::{Error, Result};
