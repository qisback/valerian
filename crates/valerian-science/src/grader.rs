// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Seestar sub-frame grader: a small neural network, run through the
//! pure-Rust `tract` ONNX runtime, that scores a raw Seestar S50 sub from 0
//! (unusable) to 1 (clean) and gives the chance of five defects.
//!
//! The model is trained outside Valerian. This module owns the contract with
//! it (see the developer page "The grader model contract" in the book): how a
//! frame is prepared, what the model must declare, and what it returns. A
//! model that breaks the contract is refused with a plain message rather than
//! graded wrongly.

use std::io::Cursor;
use std::path::Path;
use std::str::FromStr;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tract_onnx::pb::{ModelProto, tensor_shape_proto::dimension::Value as Dim, type_proto};
use tract_onnx::prelude::*;
use valerian_core::fits::Image;

use crate::{Error, Result};

/// The contract major version this build understands.
pub const CONTRACT_MAJOR: u32 = 1;
/// Raw sub height in pixels (the S50 stores its frames in portrait).
pub const SUB_ROWS: usize = 1920;
/// Raw sub width in pixels.
pub const SUB_COLS: usize = 1080;
/// Height of a superpixel plane, the model's input height.
pub const PLANE_ROWS: usize = SUB_ROWS / 2;
/// Width of a superpixel plane, the model's input width.
pub const PLANE_COLS: usize = SUB_COLS / 2;
/// Defect names in the model's output order.
pub const CLASSES: [&str; 5] = ["cloud_haze", "trail", "blur", "dew", "star_poor"];

const PLANE: usize = PLANE_ROWS * PLANE_COLS;

/// A defect the model can report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Defect {
    /// Cloud or haze across the frame.
    CloudHaze,
    /// A satellite or aircraft trail.
    Trail,
    /// Soft or trailed stars.
    Blur,
    /// Dew on the optics.
    Dew,
    /// Too few stars for the frame to be useful.
    StarPoor,
}

impl Defect {
    /// The name used in tables and on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        CLASSES[self as usize]
    }
}

impl FromStr for Defect {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        [
            Self::CloudHaze,
            Self::Trail,
            Self::Blur,
            Self::Dew,
            Self::StarPoor,
        ]
        .into_iter()
        .find(|d| d.name() == s)
        .ok_or_else(|| format!("unknown defect `{s}`; use one of {}", CLASSES.join(", ")))
    }
}

/// Probability of each defect, 0 to 1; a frame can have several.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Defects {
    /// Cloud or haze.
    pub cloud_haze: f32,
    /// Satellite or aircraft trail.
    pub trail: f32,
    /// Blur.
    pub blur: f32,
    /// Dew.
    pub dew: f32,
    /// Too few stars.
    pub star_poor: f32,
}

impl Defects {
    /// The probability of one defect.
    #[must_use]
    pub fn get(&self, d: Defect) -> f32 {
        self.as_array()[d as usize]
    }

    /// All five, in the model's output order.
    #[must_use]
    pub fn as_array(&self) -> [f32; 5] {
        [
            self.cloud_haze,
            self.trail,
            self.blur,
            self.dew,
            self.star_poor,
        ]
    }

    fn from_array(a: [f32; 5]) -> Self {
        Self {
            cloud_haze: a[0],
            trail: a[1],
            blur: a[2],
            dew: a[3],
            star_poor: a[4],
        }
    }
}

/// The model's verdict on one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Grade {
    /// Quality, 0 (unusable) to 1 (clean).
    pub score: f32,
    /// Defect probabilities.
    pub defects: Defects,
}

/// When a graded frame is rejected.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// Reject a score below this.
    pub min_score: Option<f32>,
    /// Reject a defect at or above its probability.
    pub max_defect: Vec<(Defect, f32)>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min_score: Some(0.5),
            max_defect: Vec::new(),
        }
    }
}

impl Policy {
    /// One reason per failed test; empty when the frame passes.
    #[must_use]
    pub fn reasons(&self, g: &Grade) -> Vec<String> {
        let mut why = Vec::new();
        if let Some(m) = self.min_score
            && g.score < m
        {
            why.push(format!("grade {:.2} below {m}", g.score));
        }
        for &(d, p) in &self.max_defect {
            if g.defects.get(d) >= p {
                why.push(format!(
                    "{} {:.2} at or above {p}",
                    d.name(),
                    g.defects.get(d)
                ));
            }
        }
        why
    }
}

/// Parse `NAME:P`, for example `trail:0.8`.
///
/// # Errors
/// If the name is unknown or the probability is not a number from 0 to 1.
pub fn parse_defect_limit(s: &str) -> std::result::Result<(Defect, f32), String> {
    let (name, p) = s
        .split_once(':')
        .ok_or_else(|| format!("expected NAME:P such as trail:0.8, got `{s}`"))?;
    let p: f32 = p
        .trim()
        .parse()
        .map_err(|_| format!("`{p}` is not a number"))?;
    if !(0.0..=1.0).contains(&p) {
        return Err(format!("probability {p} must be between 0 and 1"));
    }
    Ok((name.parse()?, p))
}

/// What a frame is turned into for the model.
pub struct Inputs {
    /// `[3, 960, 540]`, R then G then B, normalised.
    pub image: Vec<f32>,
    /// `[log10 median, log10 robust sigma]` of the green plane.
    pub scalars: [f32; 2],
}

/// Why a frame cannot be graded, in plain words; `None` when it can.
#[must_use]
pub fn unsuitable(img: &Image) -> Option<String> {
    if img.planes.len() != 1 {
        return Some("it is not a single-plane raw frame".into());
    }
    let p = img.plane();
    if p.width != SUB_COLS || p.height != SUB_ROWS {
        return Some(format!(
            "it is {} x {} pixels, and the grader only knows Seestar subs of {SUB_COLS} x {SUB_ROWS}",
            p.width, p.height
        ));
    }
    if p.bayer != Some(valerian_core::BayerPattern::Grbg) {
        return Some("it is not a raw GRBG Bayer frame (was it debayered or calibrated?)".into());
    }
    None
}

/// Median as numpy defines it: the mean of the two middle values for an even count.
fn median(v: &mut [f32]) -> f32 {
    let n = v.len();
    let mid = n / 2;
    let (lo, hi, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    let hi = *hi;
    if n % 2 == 1 {
        return hi;
    }
    let lo = lo.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    f64::midpoint(f64::from(lo), f64::from(hi)) as f32
}

/// Normalise one plane in place; returns its median and robust sigma.
fn normalise(plane: &mut [f32]) -> (f32, f32) {
    let m = median(&mut plane.to_vec());
    let mut dev: Vec<f32> = plane.iter().map(|v| (v - m).abs()).collect();
    let s = (1.4826 * median(&mut dev)).max(1.0);
    for v in plane.iter_mut() {
        *v = (((*v - m) / s).clamp(-20.0, 2000.0) * 0.5).asinh();
    }
    (m, s)
}

/// Prepare a frame as the contract says: superpixel split, robust asinh
/// normalisation of each plane, two scalars from the green plane.
///
/// # Errors
/// The reason, in plain words, when the frame is not a Seestar S50 raw sub.
pub fn prepare(img: &Image) -> std::result::Result<Inputs, String> {
    if let Some(why) = unsuitable(img) {
        return Err(why);
    }
    let raw = &img.plane().data;
    let mut image = vec![0.0f32; 3 * PLANE];
    for y in 0..PLANE_ROWS {
        let top = &raw[2 * y * SUB_COLS..(2 * y + 1) * SUB_COLS];
        let bottom = &raw[(2 * y + 1) * SUB_COLS..(2 * y + 2) * SUB_COLS];
        for x in 0..PLANE_COLS {
            let i = y * PLANE_COLS + x;
            image[i] = top[2 * x + 1];
            image[PLANE + i] = f32::midpoint(top[2 * x], bottom[2 * x + 1]);
            image[2 * PLANE + i] = bottom[2 * x];
        }
    }
    let mut scalars = [0.0f32; 2];
    for (c, plane) in image.as_chunks_mut::<PLANE>().0.iter_mut().enumerate() {
        let (m, s) = normalise(plane);
        if c == 1 {
            scalars = [m.max(1.0).log10(), s.max(1.0).log10()];
        }
    }
    Ok(Inputs { image, scalars })
}

/// The trained model built into the program (see the developer page "The
/// grader model contract" for replacing it).
const BUILT_IN: &[u8] = include_bytes!("../assets/seestar-subgrade-v2.onnx");

/// A loaded, contract-checked model.
pub struct Grader {
    plan: std::sync::Arc<TypedRunnableModel>,
    /// The `model_name` metadata, for the report.
    pub name: String,
}

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::Model(msg.to_string())
}

fn meta<'a>(p: &'a ModelProto, key: &str) -> Option<&'a str> {
    p.metadata_props
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.value.as_str())
}

/// Dimensions of a graph input or output; `None` for a symbolic (dynamic) one.
fn dims(v: &tract_onnx::pb::ValueInfoProto) -> Option<(i32, Vec<Option<i64>>)> {
    let Some(type_proto::Value::TensorType(t)) = v.r#type.as_ref()?.value.as_ref() else {
        return None;
    };
    let d = t
        .shape
        .as_ref()?
        .dim
        .iter()
        .map(|d| match d.value {
            Some(Dim::DimValue(n)) => Some(n),
            _ => None,
        })
        .collect();
    Some((t.elem_type, d))
}

/// Check one declared tensor against the contract.
fn check_tensor(
    p: &ModelProto,
    inputs: bool,
    index: usize,
    name: &str,
    fixed: &[i64],
) -> Result<()> {
    let kind = if inputs { "input" } else { "output" };
    let Some(g) = p.graph.as_ref() else {
        return Err(bad("the model file holds no graph"));
    };
    let list: Vec<_> = if inputs {
        let init: Vec<&str> = g.initializer.iter().map(|t| t.name.as_str()).collect();
        g.input
            .iter()
            .filter(|i| !init.contains(&i.name.as_str()))
            .collect()
    } else {
        g.output.iter().collect()
    };
    let Some(v) = list.get(index) else {
        return Err(bad(format!(
            "the model has no {kind} number {} (`{name}`)",
            index + 1
        )));
    };
    if v.name != name {
        return Err(bad(format!(
            "{kind} {} is called `{}` but the contract needs `{name}`",
            index + 1,
            v.name
        )));
    }
    let Some((elem, d)) = dims(v) else {
        return Err(bad(format!(
            "{kind} `{name}` does not declare a tensor shape"
        )));
    };
    // 1 is FLOAT in the ONNX data-type enumeration.
    if elem != 1 {
        return Err(bad(format!("{kind} `{name}` must be float32")));
    }
    let ok = d.len() == fixed.len() + 1
        && d[0].is_none_or(|n| n >= 1)
        && d[1..].iter().zip(fixed).all(|(a, b)| *a == Some(*b));
    if !ok {
        let want = fixed
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(" x ");
        return Err(bad(format!(
            "{kind} `{name}` has the wrong shape; the contract needs N x {want}"
        )));
    }
    Ok(())
}

/// Refuse a model that does not follow the contract.
fn validate(p: &ModelProto) -> Result<()> {
    let version = meta(p, "contract_version").ok_or_else(|| {
        bad("this file does not say which grader contract it follows (no `contract_version`), so it is not a Seestar grader model")
    })?;
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|m| m.trim().parse().ok())
        .ok_or_else(|| {
            bad(format!(
                "`contract_version` `{version}` is not a version number"
            ))
        })?;
    if major != CONTRACT_MAJOR {
        return Err(bad(format!(
            "this model follows grader contract {major}, but this Valerian understands contract {CONTRACT_MAJOR}; use a Valerian that matches the model"
        )));
    }
    let classes: Vec<String> = serde_json::from_str(meta(p, "classes").unwrap_or("[]"))
        .map_err(|_| bad("the model's `classes` metadata is not a list of names"))?;
    if classes != CLASSES {
        return Err(bad(format!(
            "the model's defect classes are {classes:?}, but the contract needs {CLASSES:?} in that order"
        )));
    }
    if let Some(b) = meta(p, "bayer_pattern")
        && b != "GRBG"
    {
        return Err(bad(format!(
            "the model expects Bayer pattern {b}, not GRBG"
        )));
    }
    check_tensor(
        p,
        true,
        0,
        "image",
        &[3, PLANE_ROWS as i64, PLANE_COLS as i64],
    )?;
    check_tensor(p, true, 1, "scalars", &[2])?;
    check_tensor(p, false, 0, "score", &[1])?;
    check_tensor(p, false, 1, "defects", &[5])?;
    Ok(())
}

impl Grader {
    /// Load a model from a file.
    ///
    /// # Errors
    /// If it cannot be read, is not ONNX, or breaks the contract.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        Self::from_bytes(&bytes).map_err(|e| bad(format!("{}: {e}", path.display())))
    }

    /// Load the model built into this program.
    ///
    /// # Errors
    /// If the embedded model breaks the contract (a build error, caught by the tests).
    pub fn built_in() -> Result<Self> {
        Self::from_bytes(BUILT_IN)
    }

    /// Load a model from its bytes.
    ///
    /// # Errors
    /// If it is not ONNX, or breaks the contract.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let proto = tract_onnx::onnx()
            .proto_model_for_read(&mut Cursor::new(bytes))
            .map_err(|e| bad(format!("not a readable ONNX model ({e})")))?;
        Self::from_proto(&proto)
    }

    fn from_proto(proto: &ModelProto) -> Result<Self> {
        validate(proto)?;
        let plan = tract_onnx::onnx()
            .model_for_proto_model(proto)
            .and_then(|m| {
                m.with_input_fact(0, f32::fact([1, 3, PLANE_ROWS, PLANE_COLS]).into())?
                    .with_input_fact(1, f32::fact([1, 2]).into())?
                    .into_optimized()?
                    .into_runnable()
            })
            .map_err(|e| {
                bad(format!(
                    "the runtime could not prepare the model; it may use an operator it does not support ({e})"
                ))
            })?;
        Ok(Self {
            plan,
            name: meta(proto, "model_name").unwrap_or("unnamed").to_owned(),
        })
    }

    /// Grade prepared inputs.
    ///
    /// # Errors
    /// If the runtime fails or returns something outside the contract.
    pub fn grade_inputs(&self, inputs: &Inputs) -> Result<Grade> {
        let image =
            Tensor::from_shape(&[1, 3, PLANE_ROWS, PLANE_COLS], &inputs.image).map_err(bad)?;
        let scalars = Tensor::from_shape(&[1, 2], &inputs.scalars).map_err(bad)?;
        let out = self
            .plan
            .run(tvec!(image.into(), scalars.into()))
            .map_err(|e| bad(format!("the model failed to run ({e})")))?;
        let score: Vec<f32> = out[0]
            .to_plain_array_view::<f32>()
            .map_err(bad)?
            .iter()
            .copied()
            .collect();
        let defects: Vec<f32> = out[1]
            .to_plain_array_view::<f32>()
            .map_err(bad)?
            .iter()
            .copied()
            .collect();
        if score.len() != 1 || defects.len() != 5 {
            return Err(bad("the model returned the wrong number of values"));
        }
        let d = [defects[0], defects[1], defects[2], defects[3], defects[4]];
        if !score[0].is_finite() || d.iter().any(|v| !v.is_finite()) {
            return Err(bad("the model returned a value that is not a number"));
        }
        Ok(Grade {
            score: score[0].clamp(0.0, 1.0),
            defects: Defects::from_array(d.map(|v| v.clamp(0.0, 1.0))),
        })
    }

    /// Grade one frame; `Err` holds the plain-words reason it was skipped.
    ///
    /// # Errors
    /// The frame is not a Seestar S50 raw sub, or the model failed.
    pub fn grade(&self, img: &Image) -> std::result::Result<Grade, String> {
        let inputs = prepare(img)?;
        self.grade_inputs(&inputs).map_err(|e| e.to_string())
    }

    /// Grade many frames in parallel, keeping their order.
    #[must_use]
    pub fn grade_all(&self, images: &[Image]) -> Vec<std::result::Result<Grade, String>> {
        images.par_iter().map(|i| self.grade(i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::Header;
    use valerian_core::{BayerPattern, Frame};

    const FIXTURE: &[u8] = include_bytes!("../tests/data/untrained-seestar-subgrade.onnx");

    fn mosaic(f: impl Fn(usize, usize) -> f32) -> Image {
        let data = (0..SUB_ROWS * SUB_COLS)
            .map(|i| f(i / SUB_COLS, i % SUB_COLS))
            .collect();
        let mut fr = Frame::new(SUB_COLS, SUB_ROWS, data).expect("frame");
        fr.bayer = Some(BayerPattern::Grbg);
        Image::mono(fr, Header::new())
    }

    /// Reference of the contract's normalisation, in f64.
    fn reference(values: &[f64]) -> (Vec<f64>, f64, f64) {
        let med = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            let n = v.len();
            if n % 2 == 1 {
                v[n / 2]
            } else {
                f64::midpoint(v[n / 2 - 1], v[n / 2])
            }
        };
        let m = med(&mut values.to_vec());
        let s = (1.4826 * med(&mut values.iter().map(|v| (v - m).abs()).collect())).max(1.0);
        let y = values
            .iter()
            .map(|v| (((v - m) / s).clamp(-20.0, 2000.0) / 2.0).asinh())
            .collect();
        (y, m, s)
    }

    #[test]
    fn normalisation_matches_the_contract_on_a_synthetic_mosaic() {
        // R ramps along x, G sites differ so only their mean is 1000, B is flat.
        let img = mosaic(|r, c| match (r % 2, c % 2) {
            (0, 1) => (c / 2) as f32,
            (0, 0) => 900.0,
            (1, 1) => 1100.0,
            _ => 50.0,
        });
        let inp = prepare(&img).expect("suitable");
        let r_values: Vec<f64> = (0..PLANE).map(|i| (i % PLANE_COLS) as f64).collect();
        let (want, _, _) = reference(&r_values);
        for i in [0, 1, 269, 270, 539, PLANE + 5] {
            if i < PLANE {
                assert!((f64::from(inp.image[i]) - want[i]).abs() < 1e-5, "R {i}");
            }
        }
        // Flat green: median 1000, MAD 0 so sigma floors at 1, y is 0 everywhere.
        assert!(inp.image[PLANE..2 * PLANE].iter().all(|v| v.abs() < 1e-6));
        assert!((inp.scalars[0] - 3.0).abs() < 1e-6);
        assert!(inp.scalars[1].abs() < 1e-6);
        // Flat blue sits at zero too.
        assert!(inp.image[2 * PLANE..].iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn clamps_and_floors_sigma() {
        // A hot value far above the median is clamped to 2000 sigma-units.
        let img = mosaic(|r, c| if (r, c) == (0, 1) { 60000.0 } else { 100.0 });
        let inp = prepare(&img).expect("suitable");
        let want = (2000.0f64 / 2.0).asinh();
        assert!((f64::from(inp.image[0]) - want).abs() < 1e-4);
    }

    #[test]
    fn unsuitable_frames_are_named_not_crashed() {
        let small = Image::mono(
            {
                let mut f = Frame::new(10, 10, vec![0.0; 100]).expect("frame");
                f.bayer = Some(BayerPattern::Grbg);
                f
            },
            Header::new(),
        );
        assert!(unsuitable(&small).expect("reason").contains("10 x 10"));
        let mut rggb = mosaic(|_, _| 1.0);
        rggb.planes[0].bayer = Some(BayerPattern::Rggb);
        assert!(unsuitable(&rggb).expect("reason").contains("GRBG"));
        let mut mono = mosaic(|_, _| 1.0);
        mono.planes[0].bayer = None;
        assert!(prepare(&mono).is_err());
    }

    #[test]
    fn fixture_loads_and_grades_within_range() {
        let g = Grader::from_bytes(FIXTURE).expect("fixture follows the contract");
        let img = mosaic(|r, c| 500.0 + ((r * 7 + c * 13) % 61) as f32);
        let grade = g.grade(&img).expect("graded");
        assert!(grade.score.is_finite() && (0.0..=1.0).contains(&grade.score));
        assert!(
            grade
                .defects
                .as_array()
                .iter()
                .all(|v| (0.0..=1.0).contains(v))
        );
        let all = g.grade_all(&[img.clone(), img]);
        assert_eq!(all[0], all[1]);
    }

    fn proto() -> ModelProto {
        tract_onnx::onnx()
            .proto_model_for_read(&mut Cursor::new(FIXTURE))
            .expect("fixture parses")
    }

    fn rejected(p: &ModelProto) -> String {
        match Grader::from_proto(p) {
            Ok(_) => panic!("a model that breaks the contract was accepted"),
            Err(e) => e.to_string(),
        }
    }

    fn set(p: &mut ModelProto, key: &str, value: &str) {
        for m in &mut p.metadata_props {
            if m.key == key {
                m.value = value.into();
            }
        }
    }

    #[test]
    fn rejects_other_major_versions_and_missing_version() {
        let mut p = proto();
        set(&mut p, "contract_version", "2");
        assert!(rejected(&p).contains("contract 2"));
        set(&mut p, "contract_version", "1.7");
        assert!(Grader::from_proto(&p).is_ok(), "a minor bump is accepted");
        p.metadata_props.retain(|m| m.key != "contract_version");
        assert!(rejected(&p).contains("contract_version"));
    }

    #[test]
    fn rejects_wrong_classes_and_wrong_shapes() {
        let mut p = proto();
        set(
            &mut p,
            "classes",
            r#"["trail","cloud_haze","blur","dew","star_poor"]"#,
        );
        assert!(rejected(&p).contains("classes"));

        let mut p = proto();
        let g = p.graph.as_mut().expect("graph");
        let Some(type_proto::Value::TensorType(t)) =
            g.input[0].r#type.as_mut().and_then(|t| t.value.as_mut())
        else {
            panic!("image input is a tensor");
        };
        let dim = t
            .shape
            .as_mut()
            .expect("shape")
            .dim
            .last_mut()
            .expect("dim");
        dim.value = Some(Dim::DimValue(960));
        assert!(rejected(&p).contains("wrong shape"));

        let mut p = proto();
        p.graph.as_mut().expect("graph").input[1].name = "extras".into();
        assert!(rejected(&p).contains("scalars"));
    }

    #[test]
    fn the_built_in_model_follows_the_contract() {
        let g = Grader::built_in().expect("built-in model loads");
        assert_ne!(g.name, "");
        let img = mosaic(|r, c| 500.0 + ((r * 7 + c * 13) % 61) as f32);
        let grade = g.grade(&img).expect("graded");
        assert!((0.0..=1.0).contains(&grade.score));
    }

    /// Real subs, only when `VALERIAN_SEESTAR_SUBS` names a folder of them.
    #[test]
    fn real_subs_grade_to_sane_values() {
        let Ok(dir) = std::env::var("VALERIAN_SEESTAR_SUBS") else {
            eprintln!(
                "skipping real-sub test: set VALERIAN_SEESTAR_SUBS to a folder of Seestar subs"
            );
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("folder readable")
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.to_ascii_lowercase().starts_with("fit"))
            })
            .collect();
        files.sort();
        assert!(files.len() >= 3, "need at least three subs in {dir}");
        let g = Grader::built_in().expect("built-in model loads");
        for f in files.iter().take(3) {
            let img = valerian_core::fits::read(f).expect("sub reads");
            let grade = g.grade(&img).expect("a real sub is gradable");
            assert!((0.0..=1.0).contains(&grade.score), "{f:?}: {grade:?}");
            assert!(
                grade
                    .defects
                    .as_array()
                    .iter()
                    .all(|v| (0.0..=1.0).contains(v))
            );
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(Grader::from_bytes(b"not a model").is_err());
    }

    #[test]
    fn policy_and_limit_parsing() {
        let g = Grade {
            score: 0.3,
            defects: Defects {
                trail: 0.9,
                ..Defects::default()
            },
        };
        let p = Policy {
            min_score: Some(0.5),
            max_defect: vec![parse_defect_limit("trail:0.8").expect("parses")],
        };
        assert_eq!(p.reasons(&g).len(), 2);
        assert_eq!(
            Policy::default().reasons(&Grade { score: 0.9, ..g }).len(),
            0
        );
        assert!(parse_defect_limit("snow:0.5").is_err());
        assert!(parse_defect_limit("trail:2").is_err());
        assert!(parse_defect_limit("trail").is_err());
    }
}
