//! Human-readable numbers: decimal units ("5.78 GB", "6.66 MB/s"),
//! three significant digits, short durations ("5m", "3h 11m").

use std::time::Duration;

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub fn size(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{} {}", three_digits(value), UNITS[unit])
}

pub fn speed(bytes_per_sec: u64) -> String {
    format!("{}/s", size(bytes_per_sec))
}

/// Speed for table cells: empty when nothing moves, so idle rows stay quiet.
pub fn speed_or_blank(bytes_per_sec: u64) -> String {
    if bytes_per_sec < 1000 {
        String::new()
    } else {
        speed(bytes_per_sec)
    }
}

fn three_digits(v: f64) -> String {
    let s = if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    // "6.00" -> "6", "3.70" -> "3.7"
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

pub fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (days, hours, mins) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{secs}s")
    }
}

pub fn percent(done: u64, total: u64) -> String {
    if total == 0 {
        return String::new();
    }
    let p = done as f64 / total as f64 * 100.0;
    if done >= total {
        "100%".into()
    } else if p >= 99.9 {
        "99.9%".into()
    } else {
        let s = format!("{p:.1}");
        format!("{}%", s.trim_end_matches(".0"))
    }
}

pub fn ratio(uploaded: u64, downloaded: u64) -> String {
    if downloaded == 0 {
        return "—".into();
    }
    format!("{:.3}", uploaded as f64 / downloaded as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(5_780_000_000), "5.78 GB");
        assert_eq!(size(6_000_000_000), "6 GB");
        assert_eq!(size(3_690_000_000), "3.69 GB");
        assert_eq!(size(885_000_000), "885 MB");
        assert_eq!(size(21_900_000_000), "21.9 GB");
    }

    #[test]
    fn speeds() {
        assert_eq!(speed(6_660_000), "6.66 MB/s");
        assert_eq!(speed(636_000), "636 KB/s");
        assert_eq!(speed_or_blank(10), "");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(Duration::from_secs(300)), "5m");
        assert_eq!(duration(Duration::from_secs(3 * 3600 + 11 * 60)), "3h 11m");
        assert_eq!(duration(Duration::from_secs(42)), "42s");
        assert_eq!(duration(Duration::from_secs(2 * 86_400 + 3600)), "2d 1h");
    }

    #[test]
    fn percents() {
        assert_eq!(percent(628, 1000), "62.8%");
        assert_eq!(percent(1000, 1000), "100%");
        assert_eq!(percent(0, 1000), "0%");
        assert_eq!(percent(41, 100), "41%");
        assert_eq!(percent(1, 0), "");
    }
}
