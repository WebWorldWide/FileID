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

pub(super) fn fmt_date(secs: Option<f64>) -> Option<String> {
    secs.and_then(fileid_engine::util::display::fmt_local_day)
}

pub(super) fn glass_card() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["glass-card"])
        .build()
}

/// The most pixels a thumbnail or preview decodes on its long side, however
/// dense the display.
const MAX_DECODE_PX: i32 = 4096;
const MAX_SCALE: i32 = 4;

/// Decode size for `dim` logical pixels on a display at `scale`: sharp on
/// HiDPI, never past `MAX_DECODE_PX` (and never below `dim`).
pub(super) fn hidpi_px(dim: i32, scale: i32) -> i32 {
    dim.saturating_mul(scale.clamp(1, MAX_SCALE)).min(MAX_DECODE_PX).max(dim)
}

/// `hidpi_px` at the scale of the widget the image will be drawn in.
pub(super) fn decode_px(widget: &impl gtk::prelude::IsA<gtk::Widget>, dim: i32) -> i32 {
    use gtk::prelude::WidgetExt;
    hidpi_px(dim, widget.scale_factor())
}

#[cfg(test)]
mod tests {
    use super::hidpi_px;

    #[test]
    fn decode_sizes_follow_the_display_scale_within_a_cap() {
        assert_eq!(hidpi_px(256, 1), 256);
        assert_eq!(hidpi_px(256, 2), 512);
        assert_eq!(hidpi_px(256, 3), 768);
        assert_eq!(hidpi_px(256, 0), 256);
        assert_eq!(hidpi_px(256, 9), 1024);
        assert_eq!(hidpi_px(2400, 2), 4096);
        assert_eq!(hidpi_px(1200, 2), 2400);
        assert_eq!(hidpi_px(5000, 2), 5000);
    }
}
