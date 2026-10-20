use gtk::glib;

pub(super) fn icon_paintable(name: &str, size: i32) -> Option<gtk::IconPaintable> {
    let display = gtk::gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    Some(theme.lookup_icon(
        name,
        &[],
        size,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    ))
}

pub(super) fn icon_for_kind(kind: &str) -> &'static str {
    match kind {
        "image" => "image-x-generic-symbolic",
        "video" => "video-x-generic-symbolic",
        "audio" => "audio-x-generic-symbolic",
        "pdf" | "doc" => "x-office-document-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

pub(super) fn format_bytes(b: i64) -> String {
    file_size(u64::try_from(b).unwrap_or(0))
}

/// Finder's decimal units (macOS `ByteCountFormatter`, `.file`): 1000-based,
/// KB whole, MB one decimal, GB and up two.
pub(crate) fn file_size(bytes: u64) -> String {
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


pub(super) fn fmt_date(secs: Option<f64>) -> Option<String> {
    let s = secs?;
    let dt = glib::DateTime::from_unix_local(s as i64).ok()?;
    dt.format("%Y-%m-%d").ok().map(|g| g.to_string())
}

pub(super) fn glass_card() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["glass-card"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_finder_decimal_units() {
        assert_eq!(file_size(0), "Zero KB");
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(1_000), "1 KB");
        assert_eq!(file_size(91_400_000), "91.4 MB");
        assert_eq!(file_size(4_290_000_000), "4.29 GB");
        assert_eq!(format_bytes(-5), "Zero KB");
    }
}
