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
    let kb = b as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{kb:.0} KB")
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

pub(super) fn fmt_date(secs: Option<f64>) -> Option<String> {
    let s = secs?;
    let dt = glib::DateTime::from_unix_local(s as i64).ok()?;
    dt.format("%Y-%m-%d").ok().map(|g| g.to_string())
}

pub(super) fn glass_card() -> gtk::Box {
    // Inner padding comes from the `.glass-card` CSS (16/18); the parent box's
    // `spacing` provides the gap between cards — so no margins here.
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["glass-card"])
        .build()
}
