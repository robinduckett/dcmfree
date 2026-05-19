//! Output formatting helpers that are pure (testable without filesystem).

use bytesize::ByteSize;
use std::time::Duration;

/// Format a byte count as a human-readable string (e.g. "1.23 GB").
pub fn bytes(n: u64) -> String {
    ByteSize(n).to_string()
}

/// Format a `Duration` as a coarse-grained age string
/// (e.g. "3 days", "5 hours", "just now").
pub fn age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return "just now".to_string();
    }
    let (n, unit) = if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 86_400 {
        (secs / 3600, "hour")
    } else if secs < 604_800 {
        (secs / 86_400, "day")
    } else if secs < 2_592_000 {
        (secs / 604_800, "week")
    } else if secs < 31_536_000 {
        (secs / 2_592_000, "month")
    } else {
        (secs / 31_536_000, "year")
    };
    format!("{} {}{}", n, unit, if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_zero() {
        assert_eq!(bytes(0), "0 B");
    }

    #[test]
    fn bytes_kib_boundary() {
        let s = bytes(1024);
        assert!(s.starts_with("1"), "expected leading 1, got {s}");
        assert!(s.contains('K'), "expected K-unit suffix, got {s}");
    }

    #[test]
    fn bytes_gib() {
        let s = bytes(2_147_483_648);
        assert!(s.contains('G'), "expected G-unit suffix, got {s}");
    }

    #[test]
    fn age_just_now() {
        assert_eq!(age(Duration::from_secs(0)), "just now");
        assert_eq!(age(Duration::from_secs(59)), "just now");
    }

    #[test]
    fn age_minutes_singular() {
        assert_eq!(age(Duration::from_secs(60)), "1 minute");
    }

    #[test]
    fn age_minutes_plural() {
        assert_eq!(age(Duration::from_secs(120)), "2 minutes");
    }

    #[test]
    fn age_hours() {
        assert_eq!(age(Duration::from_secs(3600)), "1 hour");
        assert_eq!(age(Duration::from_secs(7200)), "2 hours");
    }

    #[test]
    fn age_days() {
        assert_eq!(age(Duration::from_secs(86_400)), "1 day");
        assert_eq!(age(Duration::from_secs(3 * 86_400)), "3 days");
    }

    #[test]
    fn age_weeks() {
        assert_eq!(age(Duration::from_secs(7 * 86_400)), "1 week");
        assert_eq!(age(Duration::from_secs(14 * 86_400)), "2 weeks");
    }

    #[test]
    fn age_months() {
        assert_eq!(age(Duration::from_secs(31 * 86_400)), "1 month");
    }

    #[test]
    fn age_years() {
        assert_eq!(age(Duration::from_secs(366 * 86_400)), "1 year");
    }
}
