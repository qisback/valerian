// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! FITS input and output for deep-sky work.
//!
//! Unlike the minimal reader in `helia-core`, this keeps the whole header (so
//! exposure time, temperature, binning, filter, WCS and the Bayer pattern
//! survive a round trip) and the physical pixel values (`BZERO + BSCALE * raw`,
//! in ADU, not scaled to 0..1). It reads a single image in the primary HDU:
//! `NAXIS` 2, or 3 for colour planes, with `BITPIX` 8, 16, 32, -32 or -64.
//!
//! Rows are kept in the order stored in the file (row 0 is the first row of
//! the data unit), as astropy does, so pixel coordinates agree with the Python
//! pipeline and with the `WCS` keywords. `ROWORDER` is carried in the header and
//! not interpreted. The Bayer pattern from `BAYERPAT` is applied to the array
//! as stored, shifted by `XBAYROFF` and `YBAYROFF`.
//!
//! The Seestar S50 stores its rows top down (the first stored row is the top of
//! the picture), writes no `ROWORDER` card, and writes its own `WCS` for that
//! file order. Keeping the stored order therefore gives upright previews, the
//! same solved parity as the Seestar's header and a Bayer pattern that is right
//! as `BAYERPAT` says it; flipping the rows would turn `GRBG` into `BGGR`.
//!
//! # References
//!
//! - Pence, W. D. et al. (2010), Definition of the Flexible Image Transport System (FITS), version 3.0, Astron. Astrophys. 524, A42, doi:10.1051/0004-6361/201015362.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use helia_core::{BayerPattern, Frame};

use std::sync::atomic::{AtomicBool, Ordering};

const BLOCK: usize = 2880;
const CARD: usize = 80;
/// Header blocks accepted before giving up on finding `END`.
const MAX_HEADER_BLOCKS: usize = 1024;
/// Largest accepted axis length.
const MAX_AXIS: usize = 1 << 20;
/// Largest accepted number of pixels, to refuse absurd headers before allocating.
const MAX_PIXELS: usize = 1 << 31;

/// FITS failures.
#[derive(Debug, thiserror::Error)]
pub enum FitsError {
    /// The stream ended inside the header.
    #[error("FITS header truncated")]
    TruncatedHeader,
    /// The first card is not `SIMPLE  = T`.
    #[error("not a FITS file (missing SIMPLE = T)")]
    NotFits,
    /// No `END` card within the allowed size.
    #[error("FITS header has no END card within {MAX_HEADER_BLOCKS} blocks")]
    NoEnd,
    /// A required keyword is missing or malformed.
    #[error("missing or invalid FITS keyword {0}")]
    BadKeyword(&'static str),
    /// A `BITPIX` this reader does not handle.
    #[error("unsupported BITPIX {0}; supported: 8, 16, 32, -32, -64")]
    UnsupportedBitpix(i64),
    /// Not a 2-D image or a 3-plane cube.
    #[error("unsupported image shape (NAXIS = {0}); need 2-D or a 3-D cube")]
    UnsupportedShape(i64),
    /// Dimensions are zero or implausibly large.
    #[error("invalid image dimensions {width}x{height}x{planes}")]
    BadDimensions {
        /// `NAXIS1`.
        width: usize,
        /// `NAXIS2`.
        height: usize,
        /// `NAXIS3`, or 1.
        planes: usize,
    },
    /// The data unit is shorter than the header implies.
    #[error("FITS data truncated: need {expected} bytes, have {actual}")]
    TruncatedData {
        /// Bytes required.
        expected: usize,
        /// Bytes available.
        actual: usize,
    },
    /// Two images that must match in shape do not.
    #[error("image shape mismatch: {0}")]
    Mismatch(String),
    /// Reading or writing failed.
    #[error("FITS I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A header keyword value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `T` or `F`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A character string.
    Str(String),
}

impl Value {
    /// The value as a number, if it is one.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }
}

/// One header card: a keyword with a value and comment, or commentary text
/// (`HISTORY`, `COMMENT`, blank) which has no value.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// Keyword, upper case, at most eight characters.
    pub key: String,
    /// Value, or `None` for commentary cards.
    pub value: Option<Value>,
    /// Comment (for commentary cards, the text).
    pub comment: String,
}

/// An ordered FITS header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    cards: Vec<Card>,
}

/// Whether the writers keep location and device-identifying cards (off by default).
static KEEP_LOCATION: AtomicBool = AtomicBool::new(false);

/// Keep (`true`) or drop (`false`, the default) location and identifying cards
/// when files are written. Reading is never affected.
pub fn set_keep_location(keep: bool) {
    KEEP_LOCATION.store(keep, Ordering::Relaxed);
}

/// Whether [`set_keep_location`] was switched on.
#[must_use]
pub fn keep_location() -> bool {
    KEEP_LOCATION.load(Ordering::Relaxed)
}

/// Exact keywords that give away the site, the people or the unit.
const IDENTIFYING: &[&str] = &[
    "LAT-OBS",
    "LONG-OBS",
    "LON-OBS",
    "ALT-OBS",
    "ELEV-OBS",
    "LATITUDE",
    "LONGITUD",
    "LONGITUDE",
    "LAT",
    "LONG",
    "LON",
    "ELEVATIO",
    "ELEVATION",
    "OBSERVAT",
    "OBSERVER",
    "SITE",
    "SITENAME",
    "GEOLAT",
    "GEOLON",
    "GEOLONG",
    "GEOELEV",
    "GEOALT",
    "OBSLAT",
    "OBSLONG",
    "OBSLON",
    "OBSALT",
    "OBSELEV",
    "DEVICEID",
    "CAMERAID",
    "CAMID",
    "UID",
    "UUID",
    "MACADDR",
    "HOSTNAME",
    "SERNUM",
    "SERNO",
    "SERIAL",
];

/// Whether a keyword gives away where the data were taken or which unit took
/// them: site coordinates and elevation (`SITELAT`, `SITELONG`, `SITEELEV`,
/// `OBSGEO-*`, `LAT-OBS`, `LONG-OBS`, `ALT-OBS`, `GEOLAT` and similar), the
/// observatory and observer names, and device serial numbers or identifiers.
#[must_use]
pub fn is_identifying(key: &str) -> bool {
    let k = key.trim().to_ascii_uppercase();
    IDENTIFYING.contains(&k.as_str())
        || k.starts_with("SITE")
        || k.starts_with("OBSGEO")
        || k.contains("SERIA")
        || k.contains("SERNO")
        || k.contains("SERNUM")
}

/// Whether a token is a per-unit identifier: at least six alphanumerics with a
/// digit that are all hexadecimal digits, or mostly digits (`a1b2c3d4`, `12345678`).
fn is_unit_id(seg: &str) -> bool {
    let digits = seg.chars().filter(char::is_ascii_digit).count();
    seg.len() >= 6
        && digits > 0
        && seg.chars().all(|c| c.is_ascii_alphanumeric())
        && (seg.chars().all(|c| c.is_ascii_hexdigit()) || digits * 2 >= seg.len())
}

/// A bare token that is an identifier on its own: eight or more hexadecimal
/// digits with at least one letter and one digit, or `SN` followed by an id.
fn is_bare_unit_id(tok: &str) -> bool {
    let hex = tok.len() >= 8
        && tok.chars().all(|c| c.is_ascii_hexdigit())
        && tok.chars().any(|c| c.is_ascii_digit())
        && tok.chars().any(|c| c.is_ascii_alphabetic());
    let up = tok.to_ascii_uppercase();
    let sn = up
        .strip_prefix("SN")
        .is_some_and(|r| r.len() >= 4 && r.chars().all(|c| c.is_ascii_hexdigit()));
    hex || sn
}

/// Free text without per-unit identifiers: `S50_a1b2c3d4` becomes `S50` and
/// `Seestar S50 SN 1234ABCD` becomes `Seestar S50`. The model name stays.
#[must_use]
pub fn strip_unit_ids(text: &str) -> String {
    const SEPS: &[char] = &['_', '-', ':', '#'];
    let mut words: Vec<String> = Vec::new();
    let mut skip_next = false;
    for tok in text.split_whitespace() {
        let up = tok
            .trim_end_matches(&[':', '#', '.'][..])
            .to_ascii_uppercase();
        if matches!(
            up.as_str(),
            "SN" | "S/N" | "SER" | "SERIAL" | "SERIALNO" | "UID"
        ) {
            skip_next = true;
            continue;
        }
        if std::mem::take(&mut skip_next) && tok.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        if is_bare_unit_id(tok) {
            continue;
        }
        // Split at the separators, keep the segments that are not ids and
        // rejoin them with the separator that preceded each.
        let mut out = String::new();
        let mut prev = "";
        let mut start = 0;
        let bytes = tok.as_bytes();
        for i in 0..=tok.len() {
            if i < tok.len() && !SEPS.contains(&(bytes[i] as char)) {
                continue;
            }
            let seg = &tok[start..i];
            if !is_unit_id(seg) && !seg.is_empty() {
                if !out.is_empty() {
                    out.push_str(prev);
                }
                out.push_str(seg);
            }
            prev = &tok[i..(i + 1).min(tok.len())];
            start = i + 1;
        }
        if !out.is_empty() {
            words.push(out);
        }
    }
    words.join(" ")
}

/// Keywords whose strings are dates, times or coordinate-system names and so
/// are never scanned for identifiers.
fn is_plain_key(key: &str) -> bool {
    ["DATE", "TIME", "CTYPE", "CUNIT", "RADESYS", "EQUINOX"]
        .iter()
        .any(|p| key.starts_with(p))
}

/// A card with per-unit identifiers removed from its text (string value,
/// comment, or commentary line), or the card unchanged when it holds none.
fn scrub_card(c: &Card) -> Card {
    let mut out = c.clone();
    if is_plain_key(&c.key) {
        return out;
    }
    let clean = |t: &str| {
        let cleaned = strip_unit_ids(t);
        (cleaned != t.split_whitespace().collect::<Vec<_>>().join(" ")).then_some(cleaned)
    };
    if let Some(Value::Str(s)) = &c.value
        && let Some(n) = clean(s)
    {
        out.value = Some(Value::Str(n));
    }
    if (c.value.is_none() || matches!(c.value, Some(Value::Str(_))))
        && let Some(n) = clean(&c.comment)
    {
        out.comment = n;
    }
    out
}

/// Cards the writer generates itself and so never copies from a header.
const STRUCTURAL: &[&str] = &[
    "SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "NAXIS3", "EXTEND", "BZERO", "BSCALE",
    "BLANK", "END",
];

impl Header {
    /// An empty header.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// All cards in file order.
    #[must_use]
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// The first card with this keyword.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.cards
            .iter()
            .find(|c| c.key == key)
            .and_then(|c| c.value.as_ref())
    }

    /// A numeric keyword.
    #[must_use]
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    /// A string keyword, trimmed.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s.trim()),
            _ => None,
        }
    }

    /// Set a keyword, replacing the first card with that key or appending.
    pub fn set(&mut self, key: &str, value: Value, comment: &str) {
        let key = key.to_ascii_uppercase();
        if let Some(c) = self.cards.iter_mut().find(|c| c.key == key) {
            c.value = Some(value);
            if !comment.is_empty() {
                comment.clone_into(&mut c.comment);
            }
        } else {
            self.cards.push(Card {
                key,
                value: Some(value),
                comment: comment.to_owned(),
            });
        }
    }

    /// A copy without the location and identifying cards (see [`is_identifying`]).
    #[must_use]
    pub fn scrubbed(&self) -> Self {
        Self {
            cards: self
                .cards
                .iter()
                .filter(|c| !is_identifying(&c.key))
                .map(scrub_card)
                .collect(),
        }
    }

    /// Remove every card with this keyword.
    pub fn remove(&mut self, key: &str) {
        self.cards.retain(|c| c.key != key);
    }

    /// Append a `HISTORY` card (split over several if long).
    pub fn add_history(&mut self, text: &str) {
        for chunk in text.as_bytes().chunks(70) {
            self.cards.push(Card {
                key: "HISTORY".into(),
                value: None,
                comment: String::from_utf8_lossy(chunk).into_owned(),
            });
        }
    }

    /// Exposure time in seconds: `EXPTIME`, else `EXPOSURE`.
    #[must_use]
    pub fn exposure(&self) -> Option<f64> {
        self.get_f64("EXPTIME").or_else(|| self.get_f64("EXPOSURE"))
    }

    /// Sensor temperature in degrees Celsius: `CCD-TEMP`, else `CCDTEMP`.
    #[must_use]
    pub fn temperature(&self) -> Option<f64> {
        self.get_f64("CCD-TEMP").or_else(|| self.get_f64("CCDTEMP"))
    }

    /// The Bayer pattern from `BAYERPAT`, shifted by `XBAYROFF` and `YBAYROFF`.
    #[must_use]
    pub fn bayer(&self) -> Option<BayerPattern> {
        let base = match self.get_str("BAYERPAT")?.to_ascii_uppercase().as_str() {
            "RGGB" => BayerPattern::Rggb,
            "GRBG" => BayerPattern::Grbg,
            "GBRG" => BayerPattern::Gbrg,
            "BGGR" => BayerPattern::Bggr,
            _ => return None,
        };
        let xo = self.get_f64("XBAYROFF").unwrap_or(0.0) as usize & 1;
        let yo = self.get_f64("YBAYROFF").unwrap_or(0.0) as usize & 1;
        Some(shift_pattern(base, xo, yo))
    }
}

/// The pattern seen from a window whose origin is offset by `(xo, yo)`.
#[must_use]
pub fn shift_pattern(p: BayerPattern, xo: usize, yo: usize) -> BayerPattern {
    match (p.channel_at(xo, yo), p.channel_at(xo + 1, yo)) {
        (0, 1) => BayerPattern::Rggb,
        (1, 0) => BayerPattern::Grbg,
        (1, 2) => BayerPattern::Gbrg,
        _ => BayerPattern::Bggr,
    }
}

/// An image read from or to be written to a FITS file: a header and one or
/// more planes of physical pixel values.
#[derive(Clone, Debug)]
pub struct Image {
    /// The FITS header, without the structural keywords.
    pub header: Header,
    /// Pixel planes (one for mono or raw Bayer data, three for RGB).
    pub planes: Vec<Frame>,
}

impl Image {
    /// A single-plane image.
    #[must_use]
    pub fn mono(frame: Frame, header: Header) -> Self {
        Self {
            header,
            planes: vec![frame],
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.planes.first().map_or(0, |p| p.width)
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        self.planes.first().map_or(0, |p| p.height)
    }

    /// The first plane.
    ///
    /// # Panics
    /// If the image has no planes, which no constructor produces.
    #[must_use]
    pub fn plane(&self) -> &Frame {
        &self.planes[0]
    }
}

fn parse_value(text: &str) -> Option<Value> {
    let s = text.trim_start();
    if let Some(rest) = s.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    break;
                }
            } else {
                out.push(c);
            }
        }
        return Some(Value::Str(out.trim_end().to_owned()));
    }
    let tok = s.split('/').next().unwrap_or("").trim();
    match tok {
        "T" => return Some(Value::Bool(true)),
        "F" => return Some(Value::Bool(false)),
        "" => return None,
        _ => {}
    }
    if let Ok(i) = tok.parse::<i64>() {
        return Some(Value::Int(i));
    }
    tok.replace(['D', 'd'], "E")
        .parse::<f64>()
        .ok()
        .map(Value::Float)
}

/// Comment part of a value field (after the first `/` outside quotes).
fn value_comment(text: &str) -> String {
    let mut in_str = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                if in_str && bytes.get(i + 1) == Some(&b'\'') {
                    i += 1;
                } else {
                    in_str = !in_str;
                }
            }
            b'/' if !in_str => return text[i + 1..].trim().to_owned(),
            _ => {}
        }
        i += 1;
    }
    String::new()
}

fn parse_card(card: &[u8]) -> Card {
    let key = String::from_utf8_lossy(&card[..8]).trim().to_owned();
    if card.len() >= 10 && &card[8..10] == b"= " {
        let text = String::from_utf8_lossy(&card[10..]).into_owned();
        if let Some(value) = parse_value(&text) {
            return Card {
                key,
                value: Some(value),
                comment: value_comment(&text),
            };
        }
    }
    Card {
        key,
        value: None,
        comment: String::from_utf8_lossy(&card[8.min(card.len())..])
            .trim_end()
            .to_owned(),
    }
}

/// Parse the header at the start of `bytes`; returns it with the byte offset
/// of the data unit.
fn parse_header(bytes: &[u8]) -> Result<(Header, usize), FitsError> {
    let mut cards = Vec::new();
    let mut first = true;
    for b in 0..MAX_HEADER_BLOCKS {
        let block = bytes
            .get(b * BLOCK..(b + 1) * BLOCK)
            .ok_or(FitsError::TruncatedHeader)?;
        for raw in block.as_chunks::<CARD>().0 {
            let card = parse_card(raw);
            if first {
                if card.key != "SIMPLE" || card.value != Some(Value::Bool(true)) {
                    return Err(FitsError::NotFits);
                }
                first = false;
            }
            if card.key == "END" {
                return Ok((Header { cards }, (b + 1) * BLOCK));
            }
            if !(card.key.is_empty() && card.value.is_none() && card.comment.is_empty()) {
                cards.push(card);
            }
        }
    }
    Err(FitsError::NoEnd)
}

fn int_key(h: &Header, key: &'static str) -> Result<i64, FitsError> {
    match h.get(key) {
        Some(Value::Int(i)) => Ok(*i),
        Some(Value::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        _ => Err(FitsError::BadKeyword(key)),
    }
}

/// Where and how the pixels of a FITS image are stored.
struct Layout {
    width: usize,
    height: usize,
    planes: usize,
    bitpix: i64,
    bytes_per: usize,
    bzero: f64,
    bscale: f64,
    bayer: Option<BayerPattern>,
}

impl Layout {
    fn of(header: &Header) -> Result<Self, FitsError> {
        let bitpix = int_key(header, "BITPIX")?;
        let naxis = int_key(header, "NAXIS")?;
        if !(2..=3).contains(&naxis) {
            return Err(FitsError::UnsupportedShape(naxis));
        }
        let dim = |key: &'static str| -> Result<usize, FitsError> {
            usize::try_from(int_key(header, key)?).map_err(|_| FitsError::BadKeyword(key))
        };
        let width = dim("NAXIS1")?;
        let height = dim("NAXIS2")?;
        let planes = if naxis == 3 { dim("NAXIS3")? } else { 1 };
        if width == 0
            || height == 0
            || planes == 0
            || width > MAX_AXIS
            || height > MAX_AXIS
            || planes > 64
            || width.saturating_mul(height).saturating_mul(planes) > MAX_PIXELS
        {
            return Err(FitsError::BadDimensions {
                width,
                height,
                planes,
            });
        }
        let bytes_per = match bitpix {
            8 => 1,
            16 => 2,
            32 | -32 => 4,
            -64 => 8,
            other => return Err(FitsError::UnsupportedBitpix(other)),
        };
        Ok(Self {
            width,
            height,
            planes,
            bitpix,
            bytes_per,
            bzero: header.get_f64("BZERO").unwrap_or(0.0),
            bscale: header.get_f64("BSCALE").unwrap_or(1.0),
            bayer: if naxis == 2 { header.bayer() } else { None },
        })
    }

    /// Bytes of one plane.
    fn plane_bytes(&self) -> usize {
        self.width * self.height * self.bytes_per
    }

    /// Physical values of big-endian samples.
    fn decode(&self, chunk: &[u8]) -> Vec<f32> {
        let raw = chunk.chunks_exact(self.bytes_per);
        let (bzero, bscale) = (self.bzero, self.bscale);
        let phys = |v: f64| (bzero + bscale * v) as f32;
        match self.bitpix {
            8 => raw.map(|b| phys(f64::from(b[0]))).collect(),
            16 => raw
                .map(|b| phys(f64::from(i16::from_be_bytes([b[0], b[1]]))))
                .collect(),
            32 => raw
                .map(|b| phys(f64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))))
                .collect(),
            -32 => raw
                .map(|b| phys(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))))
                .collect(),
            _ => raw
                .map(|b| {
                    phys(f64::from_be_bytes([
                        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                    ]))
                })
                .collect(),
        }
    }
}

/// Parse a FITS file held in memory.
///
/// # Errors
/// [`FitsError`] when the data is not a supported FITS image.
pub fn parse(bytes: &[u8]) -> Result<Image, FitsError> {
    let (header, data_at) = parse_header(bytes)?;
    let layout = Layout::of(&header)?;
    let n = layout.plane_bytes();
    let expected = n * layout.planes;
    let data = bytes.get(data_at..).unwrap_or(&[]);
    if data.len() < expected {
        return Err(FitsError::TruncatedData {
            expected,
            actual: data.len(),
        });
    }
    let mut planes = Vec::with_capacity(layout.planes);
    for p in 0..layout.planes {
        let frame = Frame::new(
            layout.width,
            layout.height,
            layout.decode(&data[p * n..(p + 1) * n]),
        )
        .map_err(|e| FitsError::Mismatch(e.to_string()))?
        .with_bayer(layout.bayer);
        planes.push(frame);
    }
    let mut header = header;
    for k in STRUCTURAL {
        header.remove(k);
    }
    Ok(Image { header, planes })
}

/// Reads a FITS image a band of rows at a time, so many large frames can be
/// combined without holding them all in memory.
///
/// The file is opened once and the header parsed; [`Reader::rows`] then seeks
/// to the requested rows of one plane and decodes only those. The values are
/// the physical values [`read()`] gives.
pub struct Reader {
    file: std::fs::File,
    header: Header,
    layout: Layout,
    data_at: u64,
}

impl Reader {
    /// Open a file and read its header.
    ///
    /// # Errors
    /// [`FitsError`] when the file cannot be read, is not a supported image,
    /// or is shorter than its header says.
    pub fn open(path: &Path) -> Result<Self, FitsError> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        let mut block = [0u8; BLOCK];
        let mut found = false;
        for _ in 0..MAX_HEADER_BLOCKS {
            file.read_exact(&mut block).map_err(|e| {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    FitsError::TruncatedHeader
                } else {
                    FitsError::Io(e)
                }
            })?;
            bytes.extend_from_slice(&block);
            if block
                .as_chunks::<CARD>()
                .0
                .iter()
                .any(|c| &c[..3] == b"END" && c[3..8].iter().all(|&b| b == b' '))
            {
                found = true;
                break;
            }
        }
        if !found {
            return Err(FitsError::NoEnd);
        }
        let (mut header, data_at) = parse_header(&bytes)?;
        let layout = Layout::of(&header)?;
        let expected = (layout.plane_bytes() * layout.planes) as u64;
        let actual = file.metadata()?.len().saturating_sub(data_at as u64);
        if actual < expected {
            return Err(FitsError::TruncatedData {
                expected: expected as usize,
                actual: actual as usize,
            });
        }
        for k in STRUCTURAL {
            header.remove(k);
        }
        Ok(Self {
            file,
            header,
            layout,
            data_at: data_at as u64,
        })
    }

    /// The header, without the structural cards.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Image width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.layout.width
    }

    /// Image height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        self.layout.height
    }

    /// Number of planes.
    #[must_use]
    pub fn planes(&self) -> usize {
        self.layout.planes
    }

    /// The Bayer pattern of the whole image, for a 2-D image whose header names one.
    #[must_use]
    pub fn bayer(&self) -> Option<BayerPattern> {
        self.layout.bayer
    }

    /// Rows `y0..y1` of `plane` as a frame `width` wide and `y1 - y0` high.
    /// The frame carries no Bayer tag: its first row is not row 0 of the image.
    ///
    /// # Errors
    /// [`FitsError::Mismatch`] for a range or plane outside the image, or an
    /// I/O error.
    pub fn rows(&mut self, plane: usize, y0: usize, y1: usize) -> Result<Frame, FitsError> {
        use std::io::{Read, Seek, SeekFrom};
        let l = &self.layout;
        if plane >= l.planes || y0 >= y1 || y1 > l.height {
            return Err(FitsError::Mismatch(format!(
                "rows {y0}..{y1} of plane {plane} are outside a {}x{}x{} image",
                l.width, l.height, l.planes
            )));
        }
        let row_bytes = l.width * l.bytes_per;
        let at = self.data_at + (plane * l.plane_bytes() + y0 * row_bytes) as u64;
        let mut buf = vec![0u8; (y1 - y0) * row_bytes];
        self.file.seek(SeekFrom::Start(at))?;
        self.file.read_exact(&mut buf)?;
        Frame::new(l.width, y1 - y0, l.decode(&buf)).map_err(|e| FitsError::Mismatch(e.to_string()))
    }
}

// Read a FITS file.
///
/// # Errors
/// [`FitsError`] when the file cannot be read or is not a supported image.
pub fn read(path: &Path) -> Result<Image, FitsError> {
    parse(&std::fs::read(path)?)
}

/// Read only the header of a FITS file.
///
/// # Errors
/// [`FitsError`] when the file cannot be read or has no valid header.
pub fn read_header(path: &Path) -> Result<Header, FitsError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut block = [0u8; BLOCK];
    for _ in 0..MAX_HEADER_BLOCKS {
        f.read_exact(&mut block).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                FitsError::TruncatedHeader
            } else {
                FitsError::Io(e)
            }
        })?;
        bytes.extend_from_slice(&block);
        if block
            .as_chunks::<CARD>()
            .0
            .iter()
            .any(|c| &c[..3] == b"END" && c[3..8].iter().all(|&b| b == b' '))
        {
            return parse_header(&bytes).map(|(h, _)| h);
        }
    }
    Err(FitsError::NoEnd)
}

/// Sample format for [`write()`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Unsigned 16-bit via `BITPIX = 16`, `BZERO = 32768`; values are rounded and clamped.
    Uint16,
    /// IEEE single precision, `BITPIX = -32`.
    Float32,
}

fn card_text(key: &str, value: &Value, comment: &str) -> String {
    let v = match value {
        Value::Bool(b) => format!("{:>20}", if *b { "T" } else { "F" }),
        Value::Int(i) => format!("{i:>20}"),
        Value::Float(f) => {
            let s = format!("{f:?}").replace('e', "E");
            format!("{s:>20}")
        }
        Value::Str(s) => {
            let q = s.replace('\'', "''");
            format!("'{q:<8}'")
        }
    };
    let mut line = format!("{key:<8}= {v}");
    if !comment.is_empty() {
        let _ = write!(line, " / {comment}");
    }
    pad_card(&line)
}

fn pad_card(line: &str) -> String {
    let mut s: String = line.chars().take(CARD).collect();
    while s.len() < CARD {
        s.push(' ');
    }
    s
}

/// Serialise an image to FITS bytes.
///
/// # Errors
/// [`FitsError::Mismatch`] when the planes differ in size or there are none.
pub fn to_bytes(image: &Image, format: Format) -> Result<Vec<u8>, FitsError> {
    to_bytes_with(image, format, keep_location())
}

/// [`to_bytes`] with the choice of keeping location and identifying cards
/// given explicitly, not read from [`keep_location`].
///
/// # Errors
/// [`FitsError::Mismatch`] when the planes differ in size or there are none.
pub fn to_bytes_with(image: &Image, format: Format, keep: bool) -> Result<Vec<u8>, FitsError> {
    let first = image
        .planes
        .first()
        .ok_or_else(|| FitsError::Mismatch("no planes".into()))?;
    let (w, h) = (first.width, first.height);
    if image.planes.iter().any(|p| p.width != w || p.height != h) {
        return Err(FitsError::Mismatch("planes differ in size".into()));
    }
    let mut text = String::new();
    let mut add = |c: String| text.push_str(&c);
    add(card_text(
        "SIMPLE",
        &Value::Bool(true),
        "conforms to FITS standard",
    ));
    let bitpix = if format == Format::Uint16 { 16 } else { -32 };
    add(card_text("BITPIX", &Value::Int(bitpix), "bits per pixel"));
    let naxis = if image.planes.len() > 1 { 3 } else { 2 };
    add(card_text("NAXIS", &Value::Int(naxis), "number of axes"));
    add(card_text("NAXIS1", &Value::Int(w as i64), "width"));
    add(card_text("NAXIS2", &Value::Int(h as i64), "height"));
    if naxis == 3 {
        add(card_text(
            "NAXIS3",
            &Value::Int(image.planes.len() as i64),
            "planes",
        ));
    }
    if format == Format::Uint16 {
        add(card_text(
            "BZERO",
            &Value::Float(32768.0),
            "offset for unsigned 16-bit",
        ));
        add(card_text("BSCALE", &Value::Float(1.0), ""));
    }
    for c in image.header.cards() {
        if STRUCTURAL.contains(&c.key.as_str()) || (!keep && is_identifying(&c.key)) {
            continue;
        }
        let c = if keep { c.clone() } else { scrub_card(c) };
        match &c.value {
            Some(v) => add(card_text(&c.key, v, &c.comment)),
            None => add(pad_card(&format!("{:<8}{}", c.key, c.comment))),
        }
    }
    add(pad_card("END"));
    let mut out = text.into_bytes();
    out.resize(out.len().div_ceil(BLOCK) * BLOCK, b' ');
    for p in &image.planes {
        match format {
            Format::Float32 => {
                for &v in &p.data {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Format::Uint16 => {
                for &v in &p.data {
                    let r = (v.round().clamp(0.0, 65535.0) - 32768.0) as i16;
                    out.extend_from_slice(&r.to_be_bytes());
                }
            }
        }
    }
    out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    Ok(out)
}

/// Write an image to a FITS file.
///
/// # Errors
/// [`FitsError`] on a shape problem or an I/O failure.
pub fn write(path: &Path, image: &Image, format: Format) -> Result<(), FitsError> {
    let bytes = to_bytes(image, format)?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(w: usize, h: usize) -> Image {
        let data = (0..w * h).map(|i| (i % 997) as f32 * 3.5).collect();
        let mut header = Header::new();
        header.set("EXPTIME", Value::Float(300.0), "[s] exposure");
        header.set("FILTER", Value::Str("Ha".into()), "filter");
        header.set("BAYERPAT", Value::Str("GRBG".into()), "");
        header.set("GAIN", Value::Int(139), "");
        header.add_history("calibrated by a test");
        Image::mono(Frame::new(w, h, data).expect("frame"), header)
    }

    #[test]
    fn float_round_trip_keeps_pixels_and_header() {
        let img = sample(33, 17);
        let back = parse(&to_bytes(&img, Format::Float32).expect("write")).expect("read");
        assert_eq!(back.plane().data, img.plane().data);
        assert_eq!(back.header.exposure(), Some(300.0));
        assert_eq!(back.header.get_str("FILTER"), Some("Ha"));
        assert_eq!(back.header.get("GAIN"), Some(&Value::Int(139)));
        assert_eq!(back.header.bayer(), Some(BayerPattern::Grbg));
        assert_eq!(back.plane().bayer, Some(BayerPattern::Grbg));
        assert!(
            back.header
                .cards()
                .iter()
                .any(|c| c.key == "HISTORY" && c.comment.contains("calibrated"))
        );
    }

    #[test]
    fn uint16_round_trip_is_exact_for_integers() {
        let mut img = sample(20, 9);
        for v in &mut img.planes[0].data {
            *v = v.round();
        }
        let bytes = to_bytes(&img, Format::Uint16).expect("write");
        assert_eq!(bytes.len() % BLOCK, 0);
        let back = parse(&bytes).expect("read");
        assert_eq!(back.plane().data, img.plane().data);
        // BZERO is structural: it must not leak into the header we expose.
        assert!(back.header.get("BZERO").is_none());
    }

    #[test]
    fn colour_cube_round_trips() {
        let a = sample(8, 6).planes.remove(0);
        let mut b = a.clone();
        b.data.iter_mut().for_each(|v| *v += 1.0);
        let img = Image {
            header: Header::new(),
            planes: vec![a.clone(), b.clone(), a.clone()],
        };
        let back = parse(&to_bytes(&img, Format::Float32).expect("write")).expect("read");
        assert_eq!(back.planes.len(), 3);
        assert_eq!(back.planes[1].data, b.data);
    }

    #[test]
    fn bscale_and_bzero_give_physical_values() {
        let mut h = String::new();
        for c in [
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                    2",
            "NAXIS2  =                    1",
            "BZERO   =                100.0",
            "BSCALE  =                  2.0",
            "END",
        ] {
            h.push_str(&pad_card(c));
        }
        let mut b = h.into_bytes();
        b.resize(BLOCK, b' ');
        b.extend_from_slice(&5i16.to_be_bytes());
        b.extend_from_slice(&(-7i16).to_be_bytes());
        b.resize(2 * BLOCK, 0);
        let got = parse(&b).expect("read");
        assert_eq!(got.plane().data, vec![110.0, 86.0]);
    }

    #[test]
    fn malformed_input_is_refused() {
        assert!(matches!(parse(b"hello"), Err(FitsError::TruncatedHeader)));
        let mut junk = vec![b' '; BLOCK];
        junk[..8].copy_from_slice(b"NOTFITS ");
        assert!(matches!(parse(&junk), Err(FitsError::NotFits)));
        let img = sample(10, 10);
        let bytes = to_bytes(&img, Format::Float32).expect("write");
        assert!(matches!(
            parse(&bytes[..BLOCK + 8]),
            Err(FitsError::TruncatedData { .. })
        ));
    }

    #[test]
    fn strings_with_quotes_and_slashes_survive() {
        let mut img = sample(4, 4);
        img.header
            .set("OBJECT", Value::Str("Barnard's / Star".into()), "target");
        let back = parse(&to_bytes(&img, Format::Float32).expect("write")).expect("read");
        assert_eq!(back.header.get_str("OBJECT"), Some("Barnard's / Star"));
    }

    /// A Seestar S50 style mosaic: 16-bit with `BZERO`, `GRBG`, no `ROWORDER`,
    /// rows top down. The first stored row carries red-only markers.
    fn seestar_style() -> Vec<u8> {
        let (w, h) = (8usize, 6usize);
        let mut bytes = Vec::new();
        for (k, v) in [
            ("SIMPLE", "T"),
            ("BITPIX", "16"),
            ("NAXIS", "2"),
            ("NAXIS1", "8"),
            ("NAXIS2", "6"),
            ("BZERO", "32768"),
            ("BSCALE", "1"),
            ("BAYERPAT", "'GRBG    '"),
            ("INSTRUME", "'Seestar S50'"),
        ] {
            bytes.extend_from_slice(format!("{k:<8}= {v:>20}").as_bytes());
            bytes.resize(bytes.len().div_ceil(80) * 80, b' ');
        }
        bytes.extend_from_slice(format!("{:<80}", "END").as_bytes());
        bytes.resize(bytes.len().div_ceil(2880) * 2880, b' ');
        for y in 0..h {
            for x in 0..w {
                // GRBG: row 0 is G R G R, row 1 is B G B G. The red sites of
                // row 0 (x odd) are bright; nothing else is.
                let v: i32 = if y == 0 && x % 2 == 1 { 20000 } else { 1000 };
                bytes.extend_from_slice(&((v - 32768) as i16).to_be_bytes());
            }
        }
        bytes.resize(bytes.len().div_ceil(2880) * 2880, 0);
        bytes
    }

    #[test]
    fn seestar_rows_stay_top_down_with_the_pattern_as_written() {
        let img = parse(&seestar_style()).expect("read");
        assert!(img.header.get("ROWORDER").is_none());
        let f = img.plane();
        assert_eq!(f.bayer, Some(BayerPattern::Grbg));
        assert_eq!(
            f.bayer.map(|p| p.channel_at(1, 0)),
            Some(0),
            "red at x=1, y=0"
        );
        // The first stored row is row 0, not the last.
        assert_eq!(f.data[1], 20000.0);
        assert_eq!(f.data[5 * 8 + 1], 1000.0);
        // Demosaiced, the red markers are at the top of the picture.
        let [r, _, _] =
            crate::debayer::demosaic(f, crate::debayer::Method::Bilinear).expect("demosaic");
        let top: f32 = r.data[..8].iter().sum();
        let bottom: f32 = r.data[5 * 8..].iter().sum();
        assert!(top > 4.0 * bottom, "top {top} bottom {bottom}");
    }

    #[test]
    fn bayer_offsets_shift_the_pattern() {
        let mut h = Header::new();
        h.set("BAYERPAT", Value::Str("RGGB".into()), "");
        assert_eq!(h.bayer(), Some(BayerPattern::Rggb));
        h.set("XBAYROFF", Value::Int(1), "");
        assert_eq!(h.bayer(), Some(BayerPattern::Grbg));
        h.set("YBAYROFF", Value::Int(1), "");
        assert_eq!(h.bayer(), Some(BayerPattern::Bggr));
    }
}
