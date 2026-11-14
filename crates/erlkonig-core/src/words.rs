//! Times for people: "3 minutes ago", and RFC 3339 for files.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How long ago `secs` seconds was, in words.
pub fn ago(secs: u64) -> String {
    fn n(v: u64, one: &str, many: &str) -> String {
        if v == 1 { one.to_string() } else { format!("{v} {many} ago") }
    }
    match secs {
        0..=9 => "just now".into(),
        10..=59 => format!("{secs} seconds ago"),
        60..=3599 => n(secs / 60, "a minute ago", "minutes"),
        3600..=86_399 => n(secs / 3600, "an hour ago", "hours"),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// `unix` seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(3), "just now");
        assert_eq!(ago(42), "42 seconds ago");
        assert_eq!(ago(61), "a minute ago");
        assert_eq!(ago(3 * 60 + 5), "3 minutes ago");
        assert_eq!(ago(3600), "an hour ago");
        assert_eq!(ago(5 * 3600), "5 hours ago");
        assert_eq!(ago(90_000), "yesterday");
        assert_eq!(ago(3 * 86_400), "3 days ago");
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_791_201_600), "2026-10-05T12:00:00Z");
    }
}
