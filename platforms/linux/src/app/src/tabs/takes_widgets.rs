// Best Takes pieces (DESIGN_V3_PORT "Best Takes"): a fixed-size shot view that
// zooms onto a face on the standard spring, the BEST / THIS TRY compare pane
// and the filmstrip tile.
//
// Shots draw through Cairo instead of gtk::Picture because a Picture asks for
// its texture's natural size, which would blow the 28 px fan thumbnails and the
// 112 × 76 tiles up to the decoded size.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::{CatalogTakeAutoGroup, CatalogTakeBox, CatalogTakeQuality};
use gtk::cairo;

use super::takes_model as model;
use super::workbenches::is_playable_media;
use crate::engine_client::DecodedImage;
use crate::v3;

#[derive(Clone)]
pub struct Shot {
    surface: cairo::ImageSurface,
    width: f64,
    height: f64,
}

impl Shot {
    pub fn from_decoded(decoded: DecodedImage) -> Option<Self> {
        let (width, height) = (decoded.width as i32, decoded.height as i32);
        if width <= 0 || height <= 0 || decoded.rgba.len() < width as usize * height as usize * 4 {
            return None;
        }
        let data = model::rgba_to_argb32(&decoded.rgba);
        let surface = cairo::ImageSurface::create_for_data(
            data,
            cairo::Format::ARgb32,
            width,
            height,
            width * 4,
        )
        .ok()?;
        Some(Self {
            surface,
            width: f64::from(width),
            height: f64::from(height),
        })
    }
}

#[derive(Clone)]
pub struct ShotView {
    pub area: gtk::DrawingArea,
    shot: Rc<RefCell<Option<Shot>>>,
    region: Rc<Cell<CatalogTakeBox>>,
    animation: Rc<RefCell<Option<adw::SpringAnimation>>>,
}

impl ShotView {
    /// `cover` fills the view (thumbnails); otherwise the shot is fitted.
    /// `ring` strokes a 1.5 px base-colour edge so fanned thumbnails separate.
    pub fn new(width: i32, height: i32, radius: f64, cover: bool, ring: bool) -> Self {
        let area = gtk::DrawingArea::builder()
            .content_width(width)
            .content_height(height)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let shot: Rc<RefCell<Option<Shot>>> = Rc::new(RefCell::new(None));
        let region = Rc::new(Cell::new(model::WHOLE));
        let (draw_shot, draw_region) = (shot.clone(), region.clone());
        area.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            let _ = cr.save();
            v3::draw_round_rect(cr, 0.0, 0.0, w, h, radius);
            cr.clip();
            if let Some(shot) = draw_shot.borrow().as_ref() {
                let (scale, x, y) = model::view_transform(
                    (shot.width, shot.height),
                    (w, h),
                    draw_region.get(),
                    cover,
                );
                cr.translate(x, y);
                cr.scale(scale, scale);
                if cr.set_source_surface(&shot.surface, 0.0, 0.0).is_ok() {
                    cr.source().set_filter(cairo::Filter::Good);
                    let _ = cr.paint();
                }
            }
            let _ = cr.restore();
            if ring {
                v3::draw_round_rect(cr, 0.75, 0.75, w - 1.5, h - 1.5, radius);
                cr.set_source_rgb(0.043, 0.043, 0.047);
                cr.set_line_width(1.5);
                let _ = cr.stroke();
            }
        });
        Self {
            area,
            shot,
            region,
            animation: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_shot(&self, shot: Option<Shot>) {
        self.shot.replace(shot);
        self.area.queue_draw();
    }

    pub fn set_region(&self, target: CatalogTakeBox, animate: bool) {
        if let Some(previous) = self.animation.take() {
            previous.pause();
        }
        let from = self.region.get();
        if !animate || from == target {
            self.region.set(target);
            self.area.queue_draw();
            return;
        }
        let (region, area) = (self.region.clone(), self.area.clone());
        let animation = crate::spring::animate(&self.area, 0.0, 1.0, move |t| {
            region.set(model::lerp_box(from, target, t));
            area.queue_draw();
        });
        self.animation.replace(Some(animation));
    }
}

/// One side of Compare: header (title, star, Keep mark), the image well
/// (with an inline player for clips) and the filename, score, bar and reasons.
pub struct ComparePane {
    pub root: gtk::Box,
    title: gtk::Label,
    star: gtk::Label,
    keep_mark: gtk::Label,
    well: gtk::Stack,
    pub view: ShotView,
    video: gtk::Video,
    pub play: gtk::Button,
    name: gtk::Label,
    raw: gtk::Label,
    score: gtk::Label,
    bar: gtk::ProgressBar,
    chips: gtk::Box,
    pub shown: Cell<Option<i64>>,
}

impl ComparePane {
    pub fn new() -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .css_classes(["v3-panel"])
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .build();
        let header = v3::hbox(8);
        header.add_css_class("v3-take-head");
        let title = v3::section("BEST");
        header.append(&title);
        let star = v3::label("★", &["v3-take-star"]);
        star.set_visible(false);
        header.append(&star);
        header.append(&v3::spacer());
        let keep_mark = v3::label("✓ Keep", &["v3-take-keep", "tone-done"]);
        keep_mark.set_visible(false);
        header.append(&keep_mark);
        root.append(&header);

        let view = ShotView::new(120, 340, 0.0, false, false);
        view.area.set_hexpand(true);
        let video = gtk::Video::builder().autoplay(false).build();
        let well = gtk::Stack::builder().vhomogeneous(false).build();
        well.add_named(&view.area, Some("image"));
        well.add_named(&video, Some("video"));
        let play = gtk::Button::builder()
            .icon_name("media-playback-start-symbolic")
            .css_classes(["v3-play-circle"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let overlay = gtk::Overlay::builder()
            .child(&well)
            .css_classes(["v3-take-well"])
            .build();
        overlay.set_size_request(-1, 340);
        overlay.add_overlay(&play);
        root.append(&overlay);

        let body = v3::vbox(8);
        body.add_css_class("v3-take-body");
        let line = v3::hbox(8);
        let name = v3::mono("", "s12");
        name.add_css_class("v3-medium");
        name.set_hexpand(true);
        line.append(&name);
        let raw = v3::label("+ RAW", &["v3-chip"]);
        raw.set_visible(false);
        line.append(&raw);
        let score = v3::mono("", "s12");
        score.add_css_class("v3-semibold");
        score.add_css_class("tone-text2");
        score.add_css_class("numeric");
        line.append(&score);
        body.append(&line);
        let bar = gtk::ProgressBar::builder()
            .css_classes(["grain-bar", "mini"])
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        body.append(&bar);
        let chips = v3::hbox(6);
        body.append(&chips);
        root.append(&body);

        Self {
            root,
            title,
            star,
            keep_mark,
            well,
            view,
            video,
            play,
            name,
            raw,
            score,
            bar,
            chips,
            shown: Cell::new(None),
        }
    }

    /// Fills the pane for `quality`. Returns true when it now shows a
    /// different try, so the caller loads its image.
    #[allow(clippy::too_many_arguments)]
    pub fn fill(
        &self,
        group: &CatalogTakeAutoGroup,
        quality: &CatalogTakeQuality,
        title: &str,
        index: usize,
        count: usize,
        best: bool,
        kept: bool,
    ) -> bool {
        self.title
            .set_text(&format!("{title} · {} of {count}", index + 1));
        toggle_class(&self.title, "tone-gold", best);
        self.star.set_visible(best);
        self.keep_mark.set_visible(kept);
        toggle_class(&self.root, "v3-take-best", best);
        let name = v3::file_name(&quality.path);
        self.name.set_text(&name);
        self.name.set_tooltip_text(Some(&quality.path));
        self.raw.set_visible(model::has_raw_twin(quality));
        let percent = model::score_percent(quality);
        self.score.set_text(&percent.to_string());
        self.score
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Score {percent} of 100"
            ))]);
        self.bar.set_fraction(quality.score.clamp(0.0, 1.0));
        toggle_class(&self.bar, "tone-gold", best);
        toggle_class(&self.bar, "tone-muted", !best);
        v3::clear(&self.chips);
        let reasons = model::reasons(quality);
        if reasons.is_empty() {
            self.chips
                .append(&v3::label("Nothing stands out", &["v3-meta-115"]));
        }
        for reason in &reasons {
            let chip = v3::label(reason, &["v3-chip"]);
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            match model::reason_tone(reason) {
                model::ReasonTone::Positive => chip.add_css_class("tone-gold"),
                model::ReasonTone::Goal => chip.add_css_class("tone-ai"),
                model::ReasonTone::Neutral => {}
            }
            self.chips.append(&chip);
        }
        self.chips
            .update_property(&[gtk::accessible::Property::Label(&reasons.join(", "))]);
        let mut spoken = title.to_lowercase();
        spoken[..1].make_ascii_uppercase();
        self.root
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{spoken}, {} {} of {count}, {name}",
                model::noun(group, 1),
                index + 1
            ))]);
        self.play.set_visible(model::is_video(group));
        self.play
            .set_tooltip_text(Some(&match quality.peak_seconds {
                Some(peak) => format!("Play from the most action, {}", v3::timecode(peak)),
                None => "Play".to_owned(),
            }));
        self.play
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Play {} {}",
                model::noun(group, 1),
                index + 1
            ))]);
        let changed =
            self.shown.replace(Some(quality.primary_file_id)) != Some(quality.primary_file_id);
        if changed {
            self.stop();
            self.view.set_shot(None);
        }
        changed
    }

    /// Plays a clip inline from one second before its most action; formats
    /// GTK can't play open in the default app.
    pub fn play(&self, quality: &CatalogTakeQuality) {
        if !is_playable_media(&quality.path) {
            v3::open_file(&self.root, &quality.path);
            return;
        }
        let media = gtk::MediaFile::for_filename(&quality.path);
        if let Some(start) = quality.peak_seconds.map(|peak| (peak - 1.0).max(0.0))
            && start > 0.0
        {
            media.connect_prepared_notify(move |stream| {
                if stream.is_prepared() && stream.is_seekable() {
                    stream.seek((start * 1_000_000.0) as i64);
                }
            });
        }
        self.video.set_media_stream(Some(&media));
        self.well.set_visible_child_name("video");
        self.play.set_visible(false);
        media.play();
    }

    pub fn stop(&self) {
        if let Some(stream) = self.video.media_stream() {
            stream.pause();
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        self.well.set_visible_child_name("image");
    }

    pub fn reset(&self) {
        self.stop();
        self.shown.set(None);
        self.view.set_shot(None);
    }
}

/// A filmstrip tile: 112 × 76 thumbnail, index badge (gold on the best) with
/// a star, a pink check when kept, and the focus / best stroke.
pub struct Tile {
    pub frame: gtk::Overlay,
    pub view: ShotView,
    badge: gtk::Label,
    star: gtk::Label,
    check: gtk::Label,
}

impl Tile {
    pub fn new(number: usize) -> Self {
        let view = ShotView::new(112, 76, 8.0, true, false);
        let frame = gtk::Overlay::builder()
            .child(&view.area)
            .css_classes(["v3-take-tile"])
            .focusable(false)
            .overflow(gtk::Overflow::Hidden)
            .valign(gtk::Align::Center)
            .build();
        let marks = v3::hbox(4);
        marks.set_halign(gtk::Align::Start);
        marks.set_valign(gtk::Align::Start);
        marks.set_margin_start(5);
        marks.set_margin_top(5);
        let badge = v3::mono(&number.to_string(), "s10");
        badge.add_css_class("v3-semibold");
        badge.add_css_class("v3-take-badge");
        marks.append(&badge);
        let star = v3::label("★", &["v3-take-star"]);
        marks.append(&star);
        frame.add_overlay(&marks);
        let check = v3::label("✓", &["v3-take-check"]);
        check.set_xalign(0.5);
        check.set_halign(gtk::Align::End);
        check.set_valign(gtk::Align::Start);
        check.set_margin_end(5);
        check.set_margin_top(5);
        frame.add_overlay(&check);
        Self {
            frame,
            view,
            badge,
            star,
            check,
        }
    }

    pub fn update(&self, best: bool, kept: bool, focused: bool, spoken: &str, hint: &str) {
        toggle_class(&self.frame, "best", best);
        toggle_class(&self.frame, "focused", focused);
        toggle_class(&self.badge, "best", best);
        self.star.set_visible(best);
        self.check.set_visible(kept);
        self.view.area.set_opacity(if kept { 1.0 } else { 0.62 });
        self.frame.update_property(&[
            gtk::accessible::Property::Label(spoken),
            gtk::accessible::Property::Description(hint),
        ]);
        self.frame
            .update_state(&[gtk::accessible::State::Selected(Some(focused))]);
    }
}

fn toggle_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}
