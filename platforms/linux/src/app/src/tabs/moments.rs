// Search & Moments (DESIGN_V3_PORT "Search & Moments"): a 300 px results
// pane, then a player panel with the Examined / Chapters / Speech timeline,
// the chapter editor, people seen in the file and the analysis queue.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::moments_timeline::{chapter_state, read_coverage, ChapterState, Coverage, Timeline};
use super::workbench_state::*;
use super::workbenches::{is_playable_media, Engine};
use crate::engine_client::{texture_from_decoded, EngineState, ThumbReply};
use crate::shell::status::Tone;
use crate::v3;

pub(super) struct ChapterRow {
    chapter: CatalogChapter,
    row: gtk::Box,
    time: gtk::Label,
}

pub(super) struct CatalogUi {
    pub(super) engine: Engine,
    pub(super) requests: RefCell<Requests>,
    pub(super) selected: RefCell<Option<CatalogHit>>,
    pub(super) editing: RefCell<Option<String>>,
    pub(super) mutation_file_id: RefCell<Option<i64>>,
    pub(super) hybrid: RefCell<HybridSearch>,
    pub(super) search_notice: gtk::Label,
    pub(super) status: gtk::Label,
    pub(super) hits: gtk::Box,
    pub(super) chapters: gtk::Box,
    pub(super) jobs: gtk::Box,
    pub(super) title: gtk::Entry,
    pub(super) summary: gtk::Entry,
    pub(super) start: gtk::SpinButton,
    pub(super) end: gtk::SpinButton,
    pub(super) root: gtk::Box,
    query: gtk::Entry,
    mode: Rc<v3::Segmented>,
    depth: Rc<v3::Segmented>,
    result_count: gtk::Label,
    searching: gtk::ProgressBar,
    detail_stack: gtk::Stack,
    player_file: gtk::Label,
    player_folder: gtk::Label,
    player_time: gtk::Label,
    media_stack: gtk::Stack,
    video: gtk::Video,
    picture: gtk::Picture,
    unplayable: gtk::Label,
    play_from_start: gtk::Button,
    timeline: Rc<Timeline>,
    chapter_panel: v3::Panel,
    draft_note: gtk::Label,
    editor: gtk::Revealer,
    editor_title: gtk::Label,
    save: gtk::Button,
    chapter_rows: RefCell<Vec<ChapterRow>>,
    people: v3::Panel,
    jobs_panel: v3::Panel,
    pending_seek: Cell<Option<f64>>,
}

impl CatalogUi {
    pub(super) fn send(&self, slot: &str, request: CatalogRequest) {
        if slot == "mutation" {
            *self.mutation_file_id.borrow_mut() = request
                .file_id
                .or_else(|| request.chapter.as_ref().map(|c| c.file_id));
        }
        self.requests.borrow_mut().begin(slot, &request.request_id);
        if slot == "search" {
            self.searching.set_visible(true);
        }
        let sent = self
            .engine
            .borrow_mut()
            .send(CommandPayload::CatalogRequest(Box::new(
                CatalogRequestPayload { request },
            )));
        if let Err(e) = sent {
            self.requests.borrow_mut().clear();
            self.searching.set_visible(false);
            self.set_status(&e.to_string());
        }
    }

    fn set_status(&self, text: &str) {
        self.status.set_label(text);
        self.status.set_visible(!text.is_empty());
    }

    pub(super) fn detail(&self) {
        if let Some(hit) = self.selected.borrow().as_ref() {
            let mut r = catalog_request("detail");
            r.file_id = Some(hit.file_id);
            self.send("detail", r);
        }
    }

    pub(super) fn is_selected(&self, file_id: i64) -> bool {
        self.selected
            .borrow()
            .as_ref()
            .is_some_and(|h| h.file_id == file_id)
    }

    fn stream(&self) -> Option<gtk::MediaStream> {
        self.video.media_stream()
    }

    fn playhead(&self) -> Option<f64> {
        self.stream()
            .filter(|stream| stream.is_prepared())
            .map(|stream| stream.timestamp() as f64 / 1_000_000.0)
    }

    pub(super) fn seek(&self, seconds: f64, play: bool) {
        match self.stream() {
            Some(stream) if stream.is_prepared() && stream.is_seekable() => {
                stream.seek((seconds.max(0.0) * 1_000_000.0) as i64);
                if play {
                    stream.play();
                }
            }
            Some(_) => self.pending_seek.set(Some(seconds)),
            None => {}
        }
        self.timeline.set_playhead(Some(seconds));
    }

    pub(super) fn select(self: &Rc<Self>, hit: CatalogHit) {
        *self.selected.borrow_mut() = Some(hit.clone());
        self.requests.borrow_mut().forget("detail");
        v3::clear(&self.chapters);
        self.chapter_rows.borrow_mut().clear();
        self.reset_editor();
        self.start.set_value(hit.start_seconds.unwrap_or(0.0));
        self.end.set_value(hit.start_seconds.unwrap_or(0.0));
        self.show_player(&hit);
        self.load_coverage(hit.file_id);
        self.detail();
        for child in children(&self.hits) {
            let selected = child.widget_name() == hit_key(&hit);
            v3::set_selected(&child, selected);
        }
    }

    fn show_player(self: &Rc<Self>, hit: &CatalogHit) {
        self.detail_stack.set_visible_child_name("detail");
        self.player_file.set_text(&v3::file_name(&hit.path));
        let folder = v3::parent_name(&hit.path);
        self.player_folder.set_text(&folder);
        self.player_folder.set_tooltip_text(Some(&folder));
        if let Some(stream) = self.stream() {
            stream.pause();
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        self.timeline.set_playhead(None);
        self.pending_seek
            .set(hit.start_seconds.filter(|s| *s > 0.0));
        let playable = hit.kind != "page"
            && is_playable_media(&hit.path)
            && std::path::Path::new(&hit.path).is_file();
        self.play_from_start.set_visible(playable);
        if playable {
            let media = gtk::MediaFile::for_filename(&hit.path);
            let weak = Rc::downgrade(self);
            media.connect_prepared_notify(move |stream| {
                let Some(ui) = weak.upgrade() else { return };
                if !stream.is_prepared() {
                    return;
                }
                let duration = stream.duration() as f64 / 1_000_000.0;
                ui.timeline
                    .set_duration((duration > 0.0).then_some(duration));
                if let Some(seconds) = ui.pending_seek.take() {
                    if stream.is_seekable() {
                        stream.seek((seconds * 1_000_000.0) as i64);
                    }
                }
            });
            self.video.set_media_stream(Some(&media));
            self.media_stack.set_visible_child_name("video");
        } else if is_image(&hit.path) {
            self.picture.set_paintable(None::<&gtk::gdk::Texture>);
            self.media_stack.set_visible_child_name("image");
            let rx = self
                .engine
                .borrow()
                .request_preview(hit.path.clone(), 1600, false);
            let picture = self.picture.downgrade();
            glib::MainContext::default().spawn_local(async move {
                if let (Ok(ThumbReply::Image(decoded)), Some(picture)) =
                    (rx.recv().await, picture.upgrade())
                {
                    picture.set_paintable(Some(&texture_from_decoded(decoded)));
                }
            });
        } else {
            self.unplayable
                .set_text(if std::path::Path::new(&hit.path).is_file() {
                    "FileID can't play this format here. Open it in its own app."
                } else {
                    "This file is offline or has moved."
                });
            self.media_stack.set_visible_child_name("unplayable");
        }
    }

    fn load_coverage(self: &Rc<Self>, file_id: i64) {
        let rx = crate::engine_client::read_db(move |conn| read_coverage(conn, file_id));
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(coverage) = rx.recv().await else {
                return;
            };
            let Some(ui) = weak.upgrade() else { return };
            if !ui.is_selected(file_id) {
                return;
            }
            ui.render_people(&coverage);
            let analyzed = !coverage.examined.is_empty();
            ui.timeline.set_coverage(coverage, analyzed);
            let known = ui
                .stream()
                .filter(|s| s.is_prepared())
                .map(|s| s.duration() as f64 / 1_000_000.0);
            ui.timeline.set_duration(known);
        });
    }

    fn render_people(self: &Rc<Self>, coverage: &Coverage) {
        v3::clear(&self.people.body);
        self.people.root.set_visible(!coverage.people.is_empty());
        for person in &coverage.people {
            let row = v3::list_row(10);
            let dot = gtk::Box::builder()
                .css_classes([
                    "v3-dot",
                    if person.reviewed {
                        "tone-gold"
                    } else {
                        "tone-ai"
                    },
                ])
                .valign(gtk::Align::Center)
                .build();
            row.append(&dot);
            let name = v3::label(&person.name, &["v3-body"]);
            name.set_hexpand(true);
            row.append(&name);
            match person.start {
                Some(start) => {
                    let play = v3::secondary(&v3::timecode(start), false);
                    play.add_css_class("v3-mono-button");
                    play.set_tooltip_text(Some(&format!("Play from {}", v3::timecode(start))));
                    let weak = Rc::downgrade(self);
                    play.connect_clicked(move |_| {
                        if let Some(ui) = weak.upgrade() {
                            ui.seek(start, true);
                        }
                    });
                    row.append(&play);
                }
                None => row.append(&v3::meta("Sampled image")),
            }
            self.people.body.append(&row);
        }
    }

    pub(super) fn render_chapters(self: &Rc<Self>, chapters: Vec<CatalogChapter>) {
        let chapters: Vec<_> = chapters
            .into_iter()
            .filter(|c| self.is_selected(c.file_id))
            .collect();
        self.timeline.set_chapters(&chapters);
        let known = self
            .stream()
            .filter(|s| s.is_prepared())
            .map(|s| s.duration() as f64 / 1_000_000.0);
        self.timeline.set_duration(known);
        self.chapter_panel
            .set_title(&format!("Chapters · {}", chapters.len()));
        self.draft_note.set_visible(
            chapters
                .iter()
                .any(|c| chapter_state(c) == ChapterState::Draft),
        );
        let mut rows = Vec::new();
        for chapter in chapters {
            rows.push(self.chapter_row(chapter));
        }
        if rows.is_empty() {
            let empty = v3::meta("No chapters yet. Analyze the video or add one.");
            empty.add_css_class("v3-row-pad");
            self.chapters.append(&empty);
        }
        self.chapter_rows.replace(rows);
        self.mark_current_chapter();
    }

    fn chapter_row(self: &Rc<Self>, chapter: CatalogChapter) -> ChapterRow {
        let state = chapter_state(&chapter);
        let row = v3::hbox(8);
        row.add_css_class("v3-chapter-row");
        row.add_css_class("v3-soft-row");
        let content = v3::hbox(10);
        let time = v3::mono(&v3::timecode(chapter.start_seconds), "s12");
        time.add_css_class("v3-medium");
        time.set_size_request(56, -1);
        time.set_valign(gtk::Align::Start);
        content.append(&time);
        let text = v3::vbox(2);
        let title = v3::label(&chapter.title, &["v3-title-13"]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        if !chapter.summary.is_empty() {
            let summary = v3::wrapped(&chapter.summary, &["v3-meta"]);
            summary.set_lines(2);
            summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&summary);
        }
        content.append(&text);
        let play = gtk::Button::builder()
            .child(&content)
            .hexpand(true)
            .css_classes(["v3-chapter-button"])
            .build();
        play.update_property(&[
            gtk::accessible::Property::Label(&format!(
                "{}, at {}",
                chapter.title,
                v3::spoken_timecode(chapter.start_seconds)
            )),
            gtk::accessible::Property::Description("Plays from this chapter"),
        ]);
        play.set_tooltip_text(Some("Plays from this chapter"));
        let start = chapter.start_seconds;
        let weak = Rc::downgrade(self);
        play.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.seek(start, true);
            }
        });
        row.append(&play);
        let (pill_text, tone) = match state {
            ChapterState::Draft => ("Draft", Tone::Ai),
            ChapterState::Accepted => ("Accepted", Tone::Done),
            ChapterState::Stale => ("Stale", Tone::Warning),
        };
        row.append(&v3::pill(pill_text, tone));
        if state == ChapterState::Draft {
            let confidence = v3::mono(
                &format!("{}%", (chapter.confidence.clamp(0.0, 1.0) * 100.0).round()),
                "s11",
            );
            confidence.add_css_class("tone-muted");
            confidence.set_size_request(34, -1);
            row.append(&confidence);
            let accept = v3::accent("Accept", Tone::Gold, false);
            let weak = Rc::downgrade(self);
            let draft = chapter.clone();
            accept.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.save_chapter(
                        &draft,
                        draft.start_seconds,
                        draft.end_seconds,
                        &draft.title,
                        &draft.summary,
                    );
                }
            });
            row.append(&accept);
        }
        let rename = v3::secondary("Rename", false);
        let weak = Rc::downgrade(self);
        let edited = chapter.clone();
        rename.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.is_selected(edited.file_id) {
                return;
            }
            *ui.editing.borrow_mut() = Some(edited.id.clone());
            ui.title.set_text(&edited.title);
            ui.summary.set_text(&edited.summary);
            ui.start.set_value(edited.start_seconds);
            ui.end.set_value(edited.end_seconds);
            ui.open_editor("Edit chapter", "Save chapter");
        });
        row.append(&rename);
        let delete = v3::icon_button("user-trash-symbolic", "Delete chapter");
        let weak = Rc::downgrade(self);
        let doomed = chapter.clone();
        delete.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.is_selected(doomed.file_id) || ui.requests.borrow().busy("mutation") {
                return;
            }
            let mut r = catalog_request("deleteChapter");
            r.file_id = Some(doomed.file_id);
            r.chapter_id = Some(doomed.id.clone());
            ui.send("mutation", r);
        });
        row.append(&delete);
        self.chapters.append(&row);
        ChapterRow { chapter, row, time }
    }

    fn mark_current_chapter(&self) {
        let playhead = self.playhead();
        for entry in self.chapter_rows.borrow().iter() {
            let inside = playhead.is_some_and(|t| {
                t >= entry.chapter.start_seconds
                    && t < entry
                        .chapter
                        .end_seconds
                        .max(entry.chapter.start_seconds + 0.5)
            });
            if inside {
                entry.row.add_css_class("v3-current");
                entry.time.add_css_class("tone-gold");
            } else {
                entry.row.remove_css_class("v3-current");
                entry.time.remove_css_class("tone-gold");
            }
        }
    }

    fn save_chapter(
        &self,
        base: &CatalogChapter,
        start: f64,
        end: f64,
        title: &str,
        summary: &str,
    ) {
        if self.requests.borrow().busy("mutation") {
            return;
        }
        let mut r = catalog_request("saveChapter");
        r.chapter = Some(CatalogChapter {
            id: base.id.clone(),
            file_id: base.file_id,
            start_seconds: start,
            end_seconds: end,
            title: title.to_owned(),
            summary: summary.to_owned(),
            source_revision: String::new(),
            model_version: "user".into(),
            confidence: 1.0,
            user_edited: true,
            stale: false,
        });
        self.send("mutation", r);
    }

    fn save_editor(&self) {
        let Some(hit) = self.selected.borrow().clone() else {
            self.set_status("Select a search result first.");
            return;
        };
        if !editor_valid(&self.title.text(), self.start.value(), self.end.value()) {
            self.set_status("Enter a title and an end time at or after the start.");
            return;
        }
        let base = CatalogChapter {
            id: self.editing.borrow().clone().unwrap_or_else(request_id),
            file_id: hit.file_id,
            start_seconds: 0.0,
            end_seconds: 0.0,
            title: String::new(),
            summary: String::new(),
            source_revision: String::new(),
            model_version: "user".into(),
            confidence: 1.0,
            user_edited: true,
            stale: false,
        };
        self.save_chapter(
            &base,
            self.start.value(),
            self.end.value(),
            &self.title.text(),
            &self.summary.text(),
        );
    }

    fn open_editor(&self, heading: &str, action: &str) {
        self.editor_title.set_text(&heading.to_uppercase());
        self.save.set_label(action);
        self.editor.set_reveal_child(true);
        self.update_save();
        self.title.grab_focus();
    }

    fn update_save(&self) {
        self.save.set_sensitive(editor_valid(
            &self.title.text(),
            self.start.value(),
            self.end.value(),
        ));
    }

    pub(super) fn reset_editor(&self) {
        *self.editing.borrow_mut() = None;
        self.title.set_text("");
        self.summary.set_text("");
        self.editor.set_reveal_child(false);
    }

    fn tick(&self) {
        let playhead = self.playhead();
        self.timeline
            .set_playhead(playhead.or(if self.stream().is_some() {
                None
            } else {
                self.timeline_hint()
            }));
        let duration = self.timeline.duration();
        self.player_time.set_text(&format!(
            "{} / {}",
            v3::timecode(playhead.unwrap_or(0.0)),
            v3::timecode(duration)
        ));
        self.mark_current_chapter();
    }

    fn timeline_hint(&self) -> Option<f64> {
        self.selected
            .borrow()
            .as_ref()
            .and_then(|hit| hit.start_seconds)
    }

    fn render_hits(self: &Rc<Self>, hits: Vec<CatalogHit>) {
        v3::clear(&self.hits);
        self.searching.set_visible(false);
        self.result_count.set_text(&format!(
            "{} result{}",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" }
        ));
        if hits.is_empty() {
            let empty = v3::meta("No matches.");
            empty.set_margin_start(14);
            self.hits.append(&empty);
        }
        for hit in hits {
            self.hits.append(&self.hit_row(hit));
        }
    }

    fn hit_row(self: &Rc<Self>, hit: CatalogHit) -> gtk::Button {
        let content = v3::vbox(2);
        let name = v3::mono(&v3::file_name(&hit.path), "s125");
        name.add_css_class("v3-medium");
        content.append(&name);
        let (kind, sampled) = hit_kind(&hit);
        let meta_text = match hit.start_seconds {
            Some(start) => format!("{} · {kind}", v3::timecode(start)),
            None => kind.to_owned(),
        };
        let meta = v3::label(&meta_text, &["v3-meta-115"]);
        if sampled {
            meta.add_css_class("tone-ai");
        }
        content.append(&meta);
        if !hit.text.trim().is_empty() {
            let snippet = v3::wrapped(hit.text.trim(), &["v3-snippet"]);
            snippet.set_lines(2);
            snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
            content.append(&snippet);
        }
        let row = v3::rail_row(&content);
        row.set_widget_name(&hit_key(&hit));
        row.update_property(&[
            gtk::accessible::Property::Label(&format!("{}, {meta_text}", v3::file_name(&hit.path))),
            gtk::accessible::Property::Description(&hit.text),
        ]);
        let weak = Rc::downgrade(self);
        row.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.select(hit.clone());
            }
        });
        row
    }

    fn render_jobs(self: &Rc<Self>, jobs: Vec<CatalogJob>) {
        v3::clear(&self.jobs);
        let active = jobs
            .iter()
            .filter(|j| matches!(j.state.as_str(), "running" | "queued" | "paused"))
            .count();
        v3::clear(&self.jobs_panel.trailing);
        self.jobs_panel.trailing.append(&v3::meta(&if active == 0 {
            "Idle".to_owned()
        } else {
            format!("{active} job{}", if active == 1 { "" } else { "s" })
        }));
        if jobs.is_empty() {
            let empty = v3::meta("Timeline and index jobs appear here.");
            empty.add_css_class("v3-row-pad");
            self.jobs.append(&empty);
        }
        for job in jobs {
            self.jobs.append(&self.job_row(job));
        }
    }

    fn job_row(self: &Rc<Self>, job: CatalogJob) -> gtk::Box {
        let row = v3::vbox(6);
        row.add_css_class("v3-job-row");
        row.add_css_class("v3-soft-row");
        let head = v3::hbox(8);
        let title = v3::label(job_title(&job.kind), &["v3-title-13"]);
        title.set_hexpand(true);
        head.append(&title);
        let (state, tone) = job_state(&job.state);
        head.append(&v3::label(state, &["v3-state", tone.css()]));
        let percent = v3::mono(
            &format!("{:.0}%", job.progress.clamp(0.0, 1.0) * 100.0),
            "s115",
        );
        head.append(&percent);
        row.append(&head);
        let files = format!(
            "{} file{}",
            job.file_ids.len(),
            if job.file_ids.len() == 1 { "" } else { "s" }
        );
        let detail = job
            .error
            .as_ref()
            .map_or(files.clone(), |error| format!("{files} · {error}"));
        let detail = v3::wrapped(&detail, &["v3-meta"]);
        row.append(&detail);
        let bar = crate::shell::GrainBar::new(true);
        bar.set(job.progress, tone);
        row.append(&bar.bar);
        let controls = v3::hbox(6);
        for (action, name) in super::workbenches::job_controls(&job) {
            let button = match name {
                "Resume" => v3::accent(name, Tone::Gold, false),
                "Cancel" => v3::ghost(name),
                _ => v3::secondary(name, false),
            };
            let weak = Rc::downgrade(self);
            let job = job.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send("job-control", super::workbenches::job_request(action, &job));
                }
            });
            controls.append(&button);
        }
        if controls.first_child().is_some() {
            row.append(&controls);
        }
        row
    }
}

fn children(container: &gtk::Box) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        out.push(widget);
    }
    out
}

fn hit_key(hit: &CatalogHit) -> String {
    format!(
        "hit-{}-{}",
        hit.file_id,
        hit.evidence_id.as_deref().unwrap_or("")
    )
}

fn is_image(path: &str) -> bool {
    use fileid_engine::pipeline::discovery::FileKind;
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(FileKind::from_extension(ext), FileKind::Image))
}

/// Result meta wording from the macOS list; true for an unverified frame.
pub(super) fn hit_kind(hit: &CatalogHit) -> (&'static str, bool) {
    match hit.kind.as_str() {
        "chapter" => ("Chapter", false),
        "sampledFrame" => ("Sampled frame, unverified", true),
        "person" => ("Person", false),
        "image" => ("Image match", false),
        "passage" if hit.page.is_some() => ("Text in document", false),
        "passage" if hit.start_seconds.is_some() => ("Passage", false),
        "passage" => ("Text in image", false),
        _ => ("Match", false),
    }
}

fn job_title(kind: &str) -> &'static str {
    match kind {
        "timelineSample" => "Timeline analysis",
        "catalogIndex" => "Catalog index",
        _ => "Catalog job",
    }
}

fn job_state(state: &str) -> (&'static str, Tone) {
    match state {
        "running" => ("Running", Tone::Gold),
        "queued" => ("Queued", Tone::Gold),
        "paused" => ("Paused", Tone::Muted),
        "completed" => ("Completed", Tone::Done),
        "failed" => ("Failed", Tone::Danger),
        "cancelled" => ("Cancelled", Tone::Danger),
        _ => ("Unknown", Tone::Muted),
    }
}

pub(super) fn editor_valid(title: &str, start: f64, end: f64) -> bool {
    !title.trim().is_empty() && end >= start
}

pub fn catalog(engine: Engine) -> gtk::Widget {
    build_catalog(engine).0
}

pub(super) fn build_catalog(engine: Engine) -> (gtk::Widget, Rc<CatalogUi>) {
    // ── Results pane ───────────────────────────────────────────────────────
    let pane = v3::vbox(0);
    pane.add_css_class("v3-pane");
    pane.set_size_request(300, -1);
    let top = v3::vbox(10);
    top.add_css_class("v3-pane-top");
    let query = gtk::Entry::builder()
        .placeholder_text("Search names, captions, chapters")
        .primary_icon_name("system-search-symbolic")
        .css_classes(["v3-field"])
        .build();
    query.update_property(&[gtk::accessible::Property::Label(
        "Search names, captions, chapters",
    )]);
    top.append(&query);
    let mode = v3::Segmented::new(&["Keyword", "Hybrid"], 0);
    mode.root.set_tooltip_text(Some(
        "Hybrid adds a local CLIP query; nothing downloads here.",
    ));
    top.append(&mode.root);
    let count_row = v3::hbox(8);
    let result_count = v3::meta("Search to find a moment");
    result_count.add_css_class("numeric");
    result_count.set_hexpand(true);
    count_row.append(&result_count);
    let searching = v3::busy_bar(80);
    count_row.append(&searching);
    top.append(&count_row);
    let search_notice = v3::wrapped("", &["v3-meta"]);
    search_notice.set_visible(false);
    top.append(&search_notice);
    pane.append(&top);
    let hits = v3::vbox(2);
    hits.set_margin_top(8);
    hits.set_margin_bottom(8);
    let hits_scroll = v3::scroller(&hits);
    pane.append(&hits_scroll);

    // ── Detail ─────────────────────────────────────────────────────────────
    let detail = v3::vbox(16);
    detail.add_css_class("v3-page-pad");
    let status = v3::wrapped("", &["v3-meta"]);
    status.set_visible(false);
    detail.append(&status);

    let player = v3::panel("Player");
    let player_time = v3::mono("0:00 / 0:00", "s12");
    player_time.add_css_class("numeric");
    player_time.add_css_class("tone-text2");
    player.trailing.append(&player_time);
    let player_body = v3::vbox(10);
    player_body.add_css_class("v3-panel-pad");
    let player_file = v3::mono("", "s13");
    player_file.add_css_class("v3-medium");
    player_body.append(&player_file);
    let player_folder = v3::mono("", "s11");
    player_folder.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    player_folder.add_css_class("tone-muted");
    player_body.append(&player_folder);
    let video = gtk::Video::builder()
        .autoplay(false)
        .css_classes(["v3-black"])
        .build();
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .css_classes(["v3-black"])
        .build();
    let unplayable_row = v3::hbox(10);
    unplayable_row.add_css_class("v3-unplayable");
    let unplayable = v3::wrapped("", &["v3-body", "tone-text2"]);
    unplayable.set_hexpand(true);
    unplayable_row.append(&unplayable);
    let open_unplayable = v3::secondary("Open", false);
    unplayable_row.append(&open_unplayable);
    let media_stack = gtk::Stack::builder().vhomogeneous(false).build();
    let framed = |child: &gtk::Widget| -> gtk::Widget {
        let frame = gtk::AspectFrame::builder()
            .ratio(16.0 / 9.0)
            .obey_child(false)
            .child(child)
            .build();
        adw::Clamp::builder()
            .maximum_size(604)
            .child(&frame)
            .halign(gtk::Align::Start)
            .build()
            .upcast()
    };
    media_stack.add_named(&framed(video.upcast_ref()), Some("video"));
    media_stack.add_named(&framed(picture.upcast_ref()), Some("image"));
    media_stack.add_named(&unplayable_row, Some("unplayable"));
    player_body.append(&media_stack);
    player.body.append(&player_body);

    let ui_slot: Rc<RefCell<Option<std::rc::Weak<CatalogUi>>>> = Rc::new(RefCell::new(None));
    let seek_slot = ui_slot.clone();
    let timeline = Timeline::new(Rc::new(move |seconds| {
        if let Some(ui) = seek_slot.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
            ui.seek(seconds, false);
        }
    }));
    timeline.root.add_css_class("v3-panel-pad");
    player.body.append(&timeline.root);
    let footer = player.show_footer();
    let depth = v3::Segmented::new(&["Quick samples", "Full moments"], 0);
    footer.append(&depth.root);
    let analyze = v3::primary("Analyze", true);
    analyze.set_tooltip_text(Some(
        "Needs a local visual model from Deep Analyze; nothing downloads automatically.",
    ));
    footer.append(&analyze);
    footer.append(&v3::spacer());
    let play_from_start = v3::secondary("Play from start", false);
    footer.append(&play_from_start);
    let reveal = v3::secondary("Show in Files", false);
    footer.append(&reveal);
    detail.append(&player.root);

    let chapter_panel = v3::panel("Chapters · 0");
    let undo = v3::ghost("Undo last edit");
    let add = v3::secondary("+ Add chapter", false);
    chapter_panel.trailing.append(&undo);
    chapter_panel.trailing.append(&add);
    let draft_note = v3::wrapped(
        "Draft chapters come from sparse samples and can miss brief events. Accept or rename each one.",
        &["v3-note-ai"],
    );
    draft_note.set_visible(false);
    chapter_panel.body.append(&draft_note);
    let editor_box = v3::vbox(8);
    editor_box.add_css_class("v3-editor");
    let editor_title = v3::section("New chapter");
    editor_box.append(&editor_title);
    let title = v3::text_field("Chapter title");
    title.add_css_class("h28");
    let summary = v3::text_field("Description");
    summary.add_css_class("h28");
    editor_box.append(&title);
    editor_box.append(&summary);
    let times = v3::hbox(8);
    let start = v3::number_field(0.0, 864_000.0, 0.1, 1);
    let end = v3::number_field(0.0, 864_000.0, 0.1, 1);
    let start_here = v3::ghost("Playhead");
    start_here.set_tooltip_text(Some("Set the start to the playhead"));
    let end_here = v3::ghost("Playhead");
    end_here.set_tooltip_text(Some("Set the end to the playhead"));
    times.append(&v3::label("Start", &["v3-meta"]));
    times.append(&start);
    times.append(&start_here);
    times.append(&v3::label("End", &["v3-meta"]));
    times.append(&end);
    times.append(&end_here);
    times.append(&v3::spacer());
    let cancel = v3::ghost("Cancel");
    cancel.set_tooltip_text(Some("Cancel (Esc)"));
    let save = v3::primary("Add chapter", true);
    save.set_tooltip_text(Some("Save (Return)"));
    times.append(&cancel);
    times.append(&save);
    editor_box.append(&times);
    let editor = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(if crate::spring::animations_enabled() {
            220
        } else {
            0
        })
        .child(&editor_box)
        .build();
    chapter_panel.body.append(&editor);
    let chapters = v3::vbox(0);
    chapter_panel.body.append(&chapters);
    chapter_panel.show_footer().append(&v3::meta(
        "Chapters live in FileID's catalog. The video file is never changed.",
    ));
    detail.append(&chapter_panel.root);

    let people = v3::panel("People");
    people.root.set_visible(false);
    detail.append(&people.root);

    let jobs_panel = v3::panel("Analysis queue");
    let jobs = v3::vbox(0);
    jobs_panel.body.append(&jobs);
    detail.append(&jobs_panel.root);

    let detail_stack = gtk::Stack::builder().vhomogeneous(false).build();
    let empty = v3::dashed_notice("Find a moment", "Search on the left, then pick a result to play it, see its timeline and review its chapters.");
    empty.add_css_class("v3-page-pad-inner");
    detail_stack.add_named(&empty, Some("empty"));
    detail_stack.add_named(&detail, Some("detail"));
    let detail_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&detail_stack)
        .build();

    let root = v3::hbox(0);
    root.append(&pane);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    root.append(&detail_scroll);

    let ui = Rc::new(CatalogUi {
        engine: engine.clone(),
        requests: RefCell::new(Requests::default()),
        selected: RefCell::new(None),
        editing: RefCell::new(None),
        mutation_file_id: RefCell::new(None),
        hybrid: RefCell::new(HybridSearch::default()),
        search_notice,
        status,
        hits,
        chapters,
        jobs,
        title,
        summary,
        start,
        end,
        root: root.clone(),
        query,
        mode,
        depth,
        result_count,
        searching,
        detail_stack,
        player_file,
        player_folder,
        player_time,
        media_stack,
        video,
        picture,
        unplayable,
        play_from_start,
        timeline,
        chapter_panel,
        draft_note,
        editor,
        editor_title,
        save,
        chapter_rows: RefCell::new(Vec::new()),
        people,
        jobs_panel,
        pending_seek: Cell::new(None),
    });
    ui_slot.replace(Some(Rc::downgrade(&ui)));
    wire(
        &ui,
        &analyze,
        &reveal,
        &open_unplayable,
        &undo,
        &add,
        &start_here,
        &end_here,
        &cancel,
    );
    subscribe(&ui);
    (root.upcast(), ui)
}

#[allow(clippy::too_many_arguments)]
fn wire(
    ui: &Rc<CatalogUi>,
    analyze: &gtk::Button,
    reveal: &gtk::Button,
    open_unplayable: &gtk::Button,
    undo: &gtk::Button,
    add: &gtk::Button,
    start_here: &gtk::Button,
    end_here: &gtk::Button,
    cancel: &gtk::Button,
) {
    let weak = Rc::downgrade(ui);
    ui.query.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            run_search(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    analyze.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.requests.borrow().busy("timeline") {
            return;
        }
        let Some(hit) = ui.selected.borrow().clone() else {
            return;
        };
        let mode = if ui.depth.selected() == 1 {
            "moments"
        } else {
            "sampled"
        };
        ui.send(
            "timeline",
            super::workbenches::timeline_request(vec![hit.file_id], mode),
        );
    });
    let weak = Rc::downgrade(ui);
    ui.play_from_start.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.seek(0.0, true);
        }
    });
    for (button, open) in [(reveal, false), (open_unplayable, true)] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |button| {
            let Some(path) = weak
                .upgrade()
                .and_then(|ui| ui.selected.borrow().as_ref().map(|h| h.path.clone()))
            else {
                return;
            };
            if open {
                v3::open_file(button, &path);
            } else {
                v3::show_in_files(button, &path);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    undo.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.requests.borrow().busy("mutation") {
            return;
        }
        let file_id = ui.selected.borrow().as_ref().map(|h| h.file_id);
        if let Some(file_id) = file_id {
            let mut r = catalog_request("undoChapterEdit");
            r.file_id = Some(file_id);
            ui.send("mutation", r);
        }
    });
    let weak = Rc::downgrade(ui);
    add.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        *ui.editing.borrow_mut() = None;
        ui.title.set_text("");
        ui.summary.set_text("");
        let at = ui.playhead().or_else(|| ui.timeline_hint()).unwrap_or(0.0);
        ui.start.set_value(at);
        ui.end.set_value(at);
        ui.open_editor("New chapter", "Add chapter");
    });
    for (button, is_start) in [(start_here, true), (end_here, false)] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let at = ui.playhead().unwrap_or(0.0);
            if is_start {
                ui.start.set_value(at);
            } else {
                ui.end.set_value(at);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    cancel.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.reset_editor();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.save.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.save_editor();
        }
    });
    for entry in [&ui.title, &ui.summary] {
        let weak = Rc::downgrade(ui);
        entry.connect_activate(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.save_editor();
            }
        });
        let weak = Rc::downgrade(ui);
        entry.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.update_save();
            }
        });
    }
    for field in [&ui.start, &ui.end] {
        let weak = Rc::downgrade(ui);
        field.connect_value_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.update_save();
            }
        });
    }
    let escape = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    escape.connect_key_pressed(move |_, key, _, _| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if key == gtk::gdk::Key::Escape && ui.editor.reveals_child() {
            ui.reset_editor();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    ui.editor.add_controller(escape);

    let weak = Rc::downgrade(ui);
    glib::timeout_add_local(Duration::from_millis(250), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if ui.root.is_mapped() && ui.selected.borrow().is_some() {
            ui.tick();
        }
        glib::ControlFlow::Continue
    });
    let weak = Rc::downgrade(ui);
    glib::timeout_add_local(Duration::from_secs(2), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if ui.root.is_mapped()
            && ui.engine.borrow().is_ready()
            && !ui.requests.borrow().busy("jobs")
        {
            ui.send("jobs", catalog_request("jobs"));
        }
        glib::ControlFlow::Continue
    });
    ui.root.connect_unmap({
        let weak = Rc::downgrade(ui);
        move |_| {
            if let Some(stream) = weak.upgrade().and_then(|ui| ui.stream()) {
                stream.pause();
            }
        }
    });
}

fn run_search(ui: &Rc<CatalogUi>) {
    let text = ui.query.text();
    if text.trim().is_empty() {
        return;
    }
    *ui.selected.borrow_mut() = None;
    ui.requests.borrow_mut().forget("detail");
    ui.reset_editor();
    v3::clear(&ui.chapters);
    v3::clear(&ui.hits);
    ui.requests.borrow_mut().forget("search");
    ui.hybrid.borrow_mut().cancel();
    if ui.mode.selected() == 0 {
        ui.search_notice.set_visible(false);
        let mut r = catalog_request("search");
        r.query = Some(text.into());
        ui.send("search", r);
        return;
    }
    let request = ui.hybrid.borrow_mut().begin(text.trim());
    let id = request.query_id.clone();
    ui.search_notice
        .set_label("Waiting for a local CLIP query embedding…");
    ui.search_notice.set_visible(true);
    ui.searching.set_visible(true);
    let sent = ui
        .engine
        .borrow_mut()
        .send(CommandPayload::EmbedTextQuery(request));
    if let Err(e) = sent {
        let fallback = ui.hybrid.borrow_mut().timeout(&id);
        if let Some(r) = fallback {
            ui.search_notice
                .set_label(&format!("Hybrid unavailable: {e}. Using keyword search."));
            ui.send("search", r);
        }
        return;
    }
    let ui = ui.clone();
    glib::timeout_add_local_once(Duration::from_secs(15), move || {
        let fallback = ui.hybrid.borrow_mut().timeout(&id);
        if let Some(r) = fallback {
            ui.search_notice
                .set_label("Hybrid embedding timed out. Using keyword search; no inference result is fabricated.");
            ui.send("search", r);
        }
    });
}

fn subscribe(ui: &Rc<CatalogUi>) {
    let rx = ui.engine.borrow_mut().subscribe();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EngineState::CatalogResponse(r) => {
                    let slot = ui.requests.borrow_mut().accept(&r.request_id, true);
                    let Some(slot) = slot else { continue };
                    if slot == "search" {
                        ui.searching.set_visible(false);
                    }
                    if r.status != "ok" {
                        ui.set_status(r.message.as_deref().unwrap_or(&r.status));
                        continue;
                    }
                    if !matches!(slot.as_str(), "jobs" | "detail") {
                        ui.set_status(r.message.as_deref().unwrap_or(""));
                    }
                    match slot.as_str() {
                        "search" => ui.render_hits(r.hits),
                        "detail" => {
                            v3::clear(&ui.chapters);
                            ui.render_chapters(r.chapters);
                        }
                        "mutation" => {
                            let owner = ui.mutation_file_id.borrow_mut().take();
                            if ui.selected.borrow().as_ref().map(|h| h.file_id) == owner {
                                ui.reset_editor();
                                ui.detail();
                            }
                        }
                        "jobs" => ui.render_jobs(r.jobs),
                        "job-control" | "timeline" => ui.send("jobs", catalog_request("jobs")),
                        _ => {}
                    }
                }
                EngineState::ClipTextEmbedding(response) => {
                    let result = ui.hybrid.borrow_mut().finish(&response);
                    if let Some((request, reason)) = result {
                        ui.search_notice.set_label(reason.as_deref().unwrap_or(
                            "Hybrid search uses a validated local CLIP embedding and the pinned CLIP model identity.",
                        ));
                        ui.send("search", request);
                    }
                }
                EngineState::Ready => ui.send("jobs", catalog_request("jobs")),
                EngineState::Failed(e) => {
                    ui.requests.borrow_mut().clear();
                    ui.searching.set_visible(false);
                    ui.set_status(&e);
                }
                EngineState::Exited => {
                    ui.requests.borrow_mut().clear();
                    ui.searching.set_visible(false);
                    ui.set_status("Engine exited; pending requests interrupted.");
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kind: &str, start: Option<f64>, page: Option<i64>) -> CatalogHit {
        CatalogHit {
            file_id: 1,
            path: "/v/clip.mp4".into(),
            kind: kind.into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: start,
            page,
        }
    }

    #[test]
    fn result_meta_names_the_evidence() {
        assert_eq!(
            hit_kind(&hit("chapter", Some(3.0), None)),
            ("Chapter", false)
        );
        assert_eq!(
            hit_kind(&hit("sampledFrame", Some(3.0), None)),
            ("Sampled frame, unverified", true)
        );
        assert_eq!(
            hit_kind(&hit("passage", None, Some(2))).0,
            "Text in document"
        );
        assert_eq!(hit_kind(&hit("passage", Some(30.0), None)).0, "Passage");
        assert_eq!(hit_kind(&hit("passage", None, None)).0, "Text in image");
    }

    #[test]
    fn the_editor_needs_a_title_and_an_ordered_range() {
        assert!(editor_valid("Intro", 0.0, 0.0));
        assert!(!editor_valid("  ", 0.0, 5.0));
        assert!(!editor_valid("Intro", 6.0, 5.0));
        assert_eq!(job_state("paused"), ("Paused", Tone::Muted));
        assert_eq!(job_title("timelineSample"), "Timeline analysis");
    }
}
