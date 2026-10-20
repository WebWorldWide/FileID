//! User-facing formatting shared by the CLI, TUI and GTK app.

/// Finder's decimal units (macOS `ByteCountFormatter`, `.file`): 1000-based,
/// KB whole, MB one decimal, GB and up two.
pub fn file_size(bytes: u64) -> String {
    const UNITS: [(&str, f64, usize); 4] = [("TB", 1e12, 2), ("GB", 1e9, 2), ("MB", 1e6, 1), ("KB", 1e3, 0)];
    if bytes == 0 {
        return "Zero KB".to_owned();
    }
    if bytes < 1000 {
        return format!("{bytes} byte{}", if bytes == 1 { "" } else { "s" });
    }
    let value = bytes as f64;
    let (unit, scale, decimals) = UNITS
        .iter()
        .copied()
        .find(|(_, scale, _)| value >= *scale)
        .unwrap_or(UNITS[3]);
    format!("{:.*} {unit}", decimals, value / scale)
}

/// [`file_size`] for signed DB columns; negatives read as zero.
pub fn file_size_i64(bytes: i64) -> String {
    file_size(u64::try_from(bytes).unwrap_or(0))
}

/// Transfer progress in the total's decimal unit: `182/271 MB`, `3.5/24.9 GB`
/// (one decimal under 100 of the unit); just [`file_size`] when the total is
/// unknown.
pub fn size_pair(done: u64, total: Option<u64>) -> String {
    const UNITS: [(&str, f64); 5] = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("KB", 1e3), ("bytes", 1.0)];
    let Some(total) = total else {
        return file_size(done);
    };
    let (unit, scale) = UNITS.iter().copied().find(|(_, scale)| total as f64 >= *scale).unwrap_or(UNITS[4]);
    let (done, total) = (done as f64 / scale, total as f64 / scale);
    let decimals = usize::from(total < 100.0 && scale > 1.0);
    format!("{done:.decimals$}/{total:.decimals$} {unit}")
}

/// `3.4 MB/s`; `—` when the rate is unknown.
pub fn rate(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "—".to_string();
    }
    format!("{}/s", file_size(bytes_per_second as u64))
}

/// Last path segment, splitting on both `/` and `\`.
pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(path)
}

/// At most `max` characters, ending in `…` when cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `"1 file"`, `"2 files"`.
pub fn plural(count: u64, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// Unix seconds as `YYYY-MM-DD HH:MM UTC`; `—` when unset or invalid.
pub fn fmt_date(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".to_string();
    }
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map_or_else(|| "—".to_string(), |time| time.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// Unix seconds as a local `YYYY-MM-DD`.
pub fn fmt_local_day(secs: f64) -> Option<String> {
    use chrono::TimeZone as _;
    if !secs.is_finite() {
        return None;
    }
    chrono::Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_finder_decimal_units() {
        assert_eq!(file_size(0), "Zero KB");
        assert_eq!(file_size(1), "1 byte");
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(1_000), "1 KB");
        assert_eq!(file_size(1_499), "1 KB");
        assert_eq!(file_size(91_400_000), "91.4 MB");
        assert_eq!(file_size(4_290_000_000), "4.29 GB");
        assert_eq!(file_size(2_500_000_000_000), "2.50 TB");
        assert_eq!(file_size_i64(-5), "Zero KB");
    }

    #[test]
    fn transfer_progress() {
        assert_eq!(size_pair(182_000_000, Some(271_000_000)), "182/271 MB");
        assert_eq!(size_pair(3_500_000_000, Some(24_900_000_000)), "3.5/24.9 GB");
        assert_eq!(size_pair(5_000_000, None), "5.0 MB");
        assert_eq!(size_pair(10, Some(500)), "10/500 bytes");
        assert_eq!(rate(3_400_000.0), "3.4 MB/s");
        assert_eq!(rate(0.0), "—");
    }

    #[test]
    fn text_helpers() {
        assert_eq!(basename("/a/b/c.txt"), "c.txt");
        assert_eq!(basename(r"C:\a\b\c.txt"), "c.txt");
        assert_eq!(basename("/a/dir/"), "dir");
        assert_eq!(basename("noslash"), "noslash");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("ab", 4), "ab");
        assert_eq!(truncate("éé", 1), "…");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(0, "file"), "0 files");
    }

    #[test]
    fn dates() {
        assert_eq!(fmt_date(0.0), "—");
        assert_eq!(fmt_date(f64::NAN), "—");
        assert_eq!(fmt_date(1_609_459_200.0), "2021-01-01 00:00 UTC");
        assert_eq!(fmt_date(1_609_459_200.0 + 3_660.0), "2021-01-01 01:01 UTC");
        assert_eq!(fmt_local_day(1_609_459_200.0).map(|day| day.len()), Some(10));
        assert!(fmt_local_day(f64::INFINITY).is_none());
    }
}
