// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `--grade`: run the Seestar sub-frame grader over raw frames.

use anyhow::{Context, Result};
use valerian_core::fits::Image;
use valerian_science::grader::{Grade, Grader, Policy};

use crate::GradeArgs;

impl GradeArgs {
    /// The rule for rejecting a graded frame.
    #[must_use]
    pub fn policy(&self) -> Policy {
        Policy {
            min_score: Some(self.min_grade),
            max_defect: self.reject_defect.clone(),
        }
    }

    fn grader(&self) -> Result<Grader> {
        if let Some(p) = &self.grader_model {
            return Grader::from_path(p).with_context(|| "cannot use the grader model");
        }
        Ok(Grader::built_in()?)
    }
}

/// Grade the frames (raw, before debayering). A frame the grader cannot
/// judge gets `None` and a note on stderr; it is never an error.
///
/// # Errors
/// If the model cannot be loaded or run.
pub fn grade_frames(
    a: &GradeArgs,
    names: &[String],
    frames: &[Image],
) -> Result<Vec<Option<Grade>>> {
    let grader = a.grader()?;
    let t = std::time::Instant::now();
    let mut skipped = 0;
    let out: Vec<Option<Grade>> = grader
        .grade_all(frames)
        .into_iter()
        .zip(names)
        .map(|(r, name)| match r {
            Ok(g) => Some(g),
            Err(why) => {
                skipped += 1;
                eprintln!("grade: {name} not graded: {why}");
                None
            }
        })
        .collect();
    eprintln!(
        "grade: {} of {} frames graded with {} in {:.1?}",
        out.len() - skipped,
        out.len(),
        grader.name,
        t.elapsed()
    );
    if skipped == out.len() {
        eprintln!("grade: none of the frames is a Seestar S50 raw sub, so nothing was graded");
    }
    Ok(out)
}
