//! Tiny RFC3339 timestamp parser shared by the capture adapters.
//!
//! Both Claude Code transcripts and Codex rollouts attach an RFC3339
//! `timestamp` to every JSONL line. We need them in two units:
//!
//! - unix seconds (i64) for [`crate::Session::created_at`]
//! - milliseconds since unix epoch (i64) for computing
//!   [`crate::Session::duration_ms`] (last − first)
//!
//! Pulling in `chrono` or `time` for this is overkill. The shapes the
//! captured agents emit are:
//!
//! - `2026-05-03T12:00:00Z`
//! - `2026-05-03T12:00:00.123Z`
//! - `2026-05-03T12:00:00.123456Z`
//! - `2026-05-03T12:00:00+00:00` / `-07:30`
//!
//! We parse all of those, return milliseconds. A malformed timestamp
//! returns `None` — callers fall back to 0 rather than failing the parse,
//! per the spec on issue #1.

/// Parse an RFC3339 timestamp into milliseconds since the unix epoch.
/// Returns `None` for any input we can't confidently parse; callers MUST
/// treat this as "no usable timestamp" and fall back gracefully.
pub(crate) fn parse_rfc3339_to_millis(s: &str) -> Option<i64> {
    // Required prefix: YYYY-MM-DDTHH:MM:SS (19 ASCII bytes). Anything
    // shorter can't be RFC3339.
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = std::str::from_utf8(bytes.get(0..4)?).ok()?.parse().ok()?;
    if bytes.get(4)? != &b'-' {
        return None;
    }
    let month: u32 = std::str::from_utf8(bytes.get(5..7)?).ok()?.parse().ok()?;
    if bytes.get(7)? != &b'-' {
        return None;
    }
    let day: u32 = std::str::from_utf8(bytes.get(8..10)?).ok()?.parse().ok()?;
    let sep = bytes.get(10)?;
    if *sep != b'T' && *sep != b't' && *sep != b' ' {
        return None;
    }
    let hour: u32 = std::str::from_utf8(bytes.get(11..13)?).ok()?.parse().ok()?;
    if bytes.get(13)? != &b':' {
        return None;
    }
    let minute: u32 = std::str::from_utf8(bytes.get(14..16)?).ok()?.parse().ok()?;
    if bytes.get(16)? != &b':' {
        return None;
    }
    let second: u32 = std::str::from_utf8(bytes.get(17..19)?).ok()?.parse().ok()?;

    // Optional fractional seconds, then required offset.
    let mut idx = 19;
    let mut millis_frac: i64 = 0;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac_str = std::str::from_utf8(&bytes[frac_start..idx]).ok()?;
        if frac_str.is_empty() {
            return None;
        }
        // Pad / truncate to exactly 3 digits for millis.
        let mut buf = [b'0'; 3];
        for (i, b) in frac_str.bytes().take(3).enumerate() {
            buf[i] = b;
        }
        millis_frac = std::str::from_utf8(&buf).ok()?.parse().ok()?;
    }

    // Offset: Z, z, or ±HH:MM (or ±HHMM as some emitters do).
    let offset_minutes: i64 = match bytes.get(idx) {
        Some(b'Z') | Some(b'z') => 0,
        Some(b'+') | Some(b'-') => {
            let sign: i64 = if bytes[idx] == b'+' { 1 } else { -1 };
            idx += 1;
            let oh: i64 = std::str::from_utf8(bytes.get(idx..idx + 2)?)
                .ok()?
                .parse()
                .ok()?;
            idx += 2;
            // Optional ':'.
            if bytes.get(idx) == Some(&b':') {
                idx += 1;
            }
            let om: i64 = std::str::from_utf8(bytes.get(idx..idx + 2)?)
                .ok()?
                .parse()
                .ok()?;
            sign * (oh * 60 + om)
        }
        _ => return None,
    };

    let unix_secs = days_from_civil(year, month, day)?
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))?
        .checked_sub(offset_minutes * 60)?;
    unix_secs.checked_mul(1000)?.checked_add(millis_frac)
}

/// Howard Hinnant's `days_from_civil` algorithm: convert a proleptic
/// Gregorian (year, month, day) to days since 1970-01-01. Returns
/// `None` only on out-of-range month/day, never on year overflow within
/// `i64`.
///
/// See <https://howardhinnant.github.io/date_algorithms.html>.
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // 0..=399
    let m = u64::from(m);
    let d = u64::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe as i64 - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_z() {
        // 2026-05-03T12:00:00Z = 1777809600 unix seconds.
        assert_eq!(
            parse_rfc3339_to_millis("2026-05-03T12:00:00Z"),
            Some(1_777_809_600_000)
        );
    }

    #[test]
    fn parses_with_millis() {
        assert_eq!(
            parse_rfc3339_to_millis("2026-05-03T12:00:00.123Z"),
            Some(1_777_809_600_123)
        );
    }

    #[test]
    fn parses_with_microseconds_truncates_to_millis() {
        assert_eq!(
            parse_rfc3339_to_millis("2026-05-03T12:00:00.123456Z"),
            Some(1_777_809_600_123)
        );
    }

    #[test]
    fn parses_with_offset() {
        // 2026-05-03T05:00:00-07:00 == 12:00:00Z
        assert_eq!(
            parse_rfc3339_to_millis("2026-05-03T05:00:00-07:00"),
            Some(1_777_809_600_000)
        );
    }

    #[test]
    fn parses_with_offset_no_colon() {
        assert_eq!(
            parse_rfc3339_to_millis("2026-05-03T05:00:00-0700"),
            Some(1_777_809_600_000)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_rfc3339_to_millis(""), None);
        assert_eq!(parse_rfc3339_to_millis("hello"), None);
        assert_eq!(parse_rfc3339_to_millis("2026-05-03"), None);
        assert_eq!(parse_rfc3339_to_millis("2026-05-03T12:00:00"), None);
    }

    #[test]
    fn epoch_anchor() {
        assert_eq!(parse_rfc3339_to_millis("1970-01-01T00:00:00Z"), Some(0));
    }
}
