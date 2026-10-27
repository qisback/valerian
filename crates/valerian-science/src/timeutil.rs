// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! FITS time stamps as seconds since the Unix epoch.

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since the Unix epoch of a `DATE-OBS` value
/// (`YYYY-MM-DDThh:mm:ss[.fff]`, or a date alone); `None` if it does not parse.
#[must_use]
pub fn parse_date_obs(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00"));
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let dd: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&dd) {
        return None;
    }
    let mut t = time.split(':');
    let h: f64 = t.next()?.parse().ok()?;
    let mi: f64 = t.next().map_or(Some(0.0), |v| v.parse().ok())?;
    let sec: f64 = t.next().map_or(Some(0.0), |v| v.parse().ok())?;
    Some(days_from_civil(y, m, dd) as f64 * 86_400.0 + h * 3600.0 + mi * 60.0 + sec)
}

/// `YYYY-MM-DDThh:mm:ss` (UTC) for seconds since the Unix epoch.
#[must_use]
pub fn format_utc(epoch: f64) -> String {
    let secs = epoch.floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_a_known_date() {
        assert_eq!(parse_date_obs("1970-01-01T00:00:00"), Some(0.0));
        // 2023-01-25T20:43:49 UTC, checked against astropy.time.
        let t = parse_date_obs("2023-01-25T20:43:49.698").expect("parse");
        assert!((t - 1_674_679_429.698).abs() < 1e-3, "{t}");
        assert_eq!(format_utc(t), "2023-01-25T20:43:49");
    }

    #[test]
    fn bad_text_is_none() {
        assert_eq!(parse_date_obs("tonight"), None);
        assert_eq!(parse_date_obs("2023-13-01T00:00:00"), None);
    }
}
