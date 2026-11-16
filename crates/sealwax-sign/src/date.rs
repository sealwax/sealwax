//! PDF dates (ISO 32000-1, 7.9.4), in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970 (UTC), now.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// `seconds` since 1970 (UTC) as a PDF date: `D:YYYYMMDDHHmmSS+00'00'`.
pub fn pdf_date(seconds: i64) -> String {
    let (year, month, day) = civil(seconds.div_euclid(86_400));
    let time = seconds.rem_euclid(86_400);
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}+00'00'",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::pdf_date;

    #[test]
    fn dates_in_utc() {
        assert_eq!(pdf_date(0), "D:19700101000000+00'00'");
        // 15 November 2026, 12:34:56 UTC.
        assert_eq!(pdf_date(1_794_746_096), "D:20261115123456+00'00'");
        assert_eq!(pdf_date(951_782_400), "D:20000229000000+00'00'");
    }
}
