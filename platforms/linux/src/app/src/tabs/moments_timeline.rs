// Search & Moments timeline (DESIGN_V3_PORT "Search & Moments", Timeline):
// Examined / Chapters / Speech lanes with a gold playhead and a ruler; a
// click on a lane seeks to x / width × duration. Coverage and speech come
// from the catalog tables read-only; chapters from the engine's detail.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::CatalogChapter;

use crate::v3;

const LANE_HEIGHT: i32 = 16;
const LABEL_WIDTH: i32 = 70;
const RULER_STEPS: [f64; 14] =
    [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Coverage {
    pub examined: Vec<(f64, f64)>,
    pub speech: Vec<(f64, f64)>,
    pub people: Vec<PersonMoment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersonMoment {
    pub name: String,
    pub start: Option<f64>,
    pub reviewed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterState {
    Accepted,
    Draft,
    Stale,
}

pub fn chapter_state(chapter: &CatalogChapter) -> ChapterState {
    if chapter.stale {
        ChapterState::Stale
    } else if chapter.user_edited || chapter.model_version == "user" {
        ChapterState::Accepted
    } else {
        ChapterState::Draft
    }
}

/// First step from the macOS list that gives at most eight ticks.
pub fn ruler_step(duration: f64) -> f64 {
    RULER_STEPS
        .iter()
        .copied()
        .find(|step| duration / step <= 8.0)
        .unwrap_or(7200.0)
}

pub fn read_coverage(conn: &rusqlite::Connection, file_id: i64) -> Coverage {
    let spans = |sql: &str| -> Vec<(f64, f64)> {
        conn.prepare(sql)
            .and_then(|mut stmt| {
                stmt.query_map([file_id], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    };
    let people = conn
        .prepare(
            "SELECT COALESCE(NULLIF(TRIM(p.name), ''), 'Unnamed person'), MIN(o.start_seconds), \
             MAX(CASE WHEN TRIM(COALESCE(p.name, '')) <> '' THEN 1 ELSE 0 END) \
             FROM catalog_observations o LEFT JOIN persons p ON p.id = o.person_id \
             WHERE o.file_id = ?1 AND o.person_id IS NOT NULL GROUP BY o.person_id \
             ORDER BY MIN(COALESCE(o.start_seconds, 1e12)), 1",
        )
        .and_then(|mut stmt| {
            stmt.query_map([file_id], |row| {
                Ok(PersonMoment { name: row.get(0)?, start: row.get(1)?, reviewed: row.get::<_, i64>(2)? == 1 })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    Coverage {
        examined: spans(
            "SELECT start_seconds, end_seconds FROM catalog_coverage \
             WHERE file_id = ?1 AND status IN ('sampled', 'verified') AND id NOT LIKE 'speech:%' \
             ORDER BY start_seconds",
        ),
        speech: spans(
            "SELECT start_seconds, COALESCE(end_seconds, start_seconds) FROM catalog_passages \
             WHERE file_id = ?1 AND stale = 0 AND id LIKE 'speech:%' AND start_seconds IS NOT NULL \
             ORDER BY start_seconds",
        ),
        people,
    }
}

#[derive(Default)]
struct State {
    duration: f64,
    playhead: Option<f64>,
    coverage: Coverage,
    chapters: Vec<(f64, f64, ChapterState)>,
    analyzed: bool,
}

pub struct Timeline {
    pub root: gtk::Box,
    state: Rc<RefCell<State>>,
    lanes: Vec<(gtk::DrawingArea, &'static str)>,
    ruler: gtk::DrawingArea,
    ruler_labels: gtk::Fixed,
}

type SeekHandler = Rc<dyn Fn(f64)>;

impl Timeline {
    pub fn new(on_seek: SeekHandler) -> Rc<Self> {
        let state = Rc::new(RefCell::new(State::default()));
        let root = v3::vbox(6);
        let mut lanes = Vec::new();
        for name in ["Examined", "Chapters", "Speech"] {
            let row = v3::hbox(10);
            let caption = v3::label(name, &["v3-lane-label"]);
            caption.set_size_request(LABEL_WIDTH, -1);
            row.append(&caption);
            let lane = gtk::DrawingArea::builder()
                .content_height(LANE_HEIGHT)
                .hexpand(true)
                .focusable(true)
                .css_classes(["v3-lane"])
                .build();
            lane.update_property(&[gtk::accessible::Property::Label(name)]);
            let draw_state = state.clone();
            lane.set_draw_func(move |_, cr, width, height| draw_lane(cr, &draw_state.borrow(), name, width, height));
            let click = gtk::GestureClick::new();
            let click_state = state.clone();
            let seek = on_seek.clone();
            let lane_ref = lane.downgrade();
            click.connect_pressed(move |_, _, x, _| {
                let Some(lane) = lane_ref.upgrade() else { return };
                let duration = click_state.borrow().duration;
                let width = f64::from(lane.width());
                if duration > 0.0 && width > 0.0 {
                    seek((x / width).clamp(0.0, 1.0) * duration);
                }
            });
            lane.add_controller(click);
            row.append(&lane);
            root.append(&row);
            lanes.push((lane, name));
        }
        let ruler_row = v3::hbox(10);
        let pad = v3::hbox(0);
        pad.set_size_request(LABEL_WIDTH, -1);
        ruler_row.append(&pad);
        let ruler = gtk::DrawingArea::builder().content_height(20).hexpand(true).can_target(false).build();
        let ruler_state = state.clone();
        ruler.set_draw_func(move |_, cr, width, _| draw_ticks(cr, ruler_state.borrow().duration, width));
        let ruler_labels = gtk::Fixed::builder().can_target(false).build();
        let ruler_stack = gtk::Overlay::builder().child(&ruler).hexpand(true).build();
        ruler_stack.add_overlay(&ruler_labels);
        ruler_row.append(&ruler_stack);
        root.append(&ruler_row);
        let timeline = Rc::new(Self { root, state, lanes, ruler, ruler_labels });
        let weak = Rc::downgrade(&timeline);
        // Re-placing labels inside the allocation pass would queue another
        // resize, so it waits for idle.
        timeline.ruler.connect_resize(move |_, _, _| {
            let weak = weak.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(timeline) = weak.upgrade() {
                    timeline.place_ruler_labels();
                }
            });
        });
        timeline
    }

    /// Mono 10.5 labels under each tick: first leading, last trailing, the
    /// rest centred.
    fn place_ruler_labels(&self) {
        while let Some(child) = self.ruler_labels.first_child() {
            self.ruler_labels.remove(&child);
        }
        let duration = self.state.borrow().duration;
        let width = f64::from(self.ruler.width());
        if duration <= 0.0 || width <= 0.0 {
            return;
        }
        let step = ruler_step(duration);
        let ticks = (duration / step).floor() as usize;
        for index in 0..=ticks {
            let seconds = index as f64 * step;
            let label = v3::mono(&v3::timecode(seconds), "s10");
            label.add_css_class("tone-muted");
            let (_, natural, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
            let text = f64::from(natural);
            let x = seconds / duration * width;
            let left = if index == 0 {
                0.0
            } else if index == ticks && x + text / 2.0 > width {
                width - text
            } else {
                (x - text / 2.0).clamp(0.0, (width - text).max(0.0))
            };
            self.ruler_labels.put(&label, left, 6.0);
        }
    }

    pub fn set_coverage(&self, coverage: Coverage, analyzed: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.coverage = coverage;
            state.analyzed = analyzed;
        }
        self.refresh();
    }

    pub fn set_chapters(&self, chapters: &[CatalogChapter]) {
        self.state.borrow_mut().chapters =
            chapters.iter().map(|c| (c.start_seconds, c.end_seconds, chapter_state(c))).collect();
        self.refresh();
    }

    /// The media duration once known, else the furthest marker.
    pub fn set_duration(&self, duration: Option<f64>) {
        {
            let mut state = self.state.borrow_mut();
            let furthest = state
                .coverage
                .examined
                .iter()
                .chain(state.coverage.speech.iter())
                .map(|(_, end)| *end)
                .chain(state.chapters.iter().map(|(_, end, _)| *end))
                .fold(0.0_f64, f64::max);
            state.duration = duration.filter(|d| *d > 0.0).unwrap_or(furthest);
        }
        self.refresh();
    }

    pub fn duration(&self) -> f64 {
        self.state.borrow().duration
    }

    pub fn set_playhead(&self, seconds: Option<f64>) {
        if self.state.borrow().playhead == seconds {
            return;
        }
        self.state.borrow_mut().playhead = seconds;
        for (lane, _) in &self.lanes {
            lane.queue_draw();
        }
    }

    fn refresh(&self) {
        let state = self.state.borrow();
        for (lane, name) in &self.lanes {
            let value = match *name {
                "Examined" if !state.analyzed && state.coverage.examined.is_empty() => "Not analyzed yet".to_owned(),
                "Examined" => count(state.coverage.examined.len(), "sampled frame"),
                "Chapters" if state.chapters.is_empty() => "None".to_owned(),
                "Chapters" => count(state.chapters.len(), "chapter"),
                _ if state.coverage.speech.is_empty() => "None".to_owned(),
                _ => count(state.coverage.speech.len(), "passage"),
            };
            lane.update_property(&[gtk::accessible::Property::Description(&value)]);
            lane.set_tooltip_text(Some(&format!("{name}: {value}")));
            lane.queue_draw();
        }
        drop(state);
        self.ruler.queue_draw();
        self.place_ruler_labels();
    }
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn draw_lane(cr: &gtk::cairo::Context, state: &State, name: &str, width: i32, height: i32) {
    let (w, h) = (f64::from(width), f64::from(height));
    v3::draw_round_rect(cr, 0.0, 0.0, w, h, 4.0);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.045);
    let _ = cr.fill();
    if state.duration <= 0.0 {
        return;
    }
    let x = |seconds: f64| (seconds / state.duration).clamp(0.0, 1.0) * w;
    match name {
        "Examined" => {
            for (start, end) in &state.coverage.examined {
                let (x0, x1) = (x(*start), x(*end));
                if x1 - x0 < 2.0 {
                    cr.set_source_rgba(0.694, 0.608, 0.808, 0.9);
                    cr.rectangle(x0, 0.0, 1.0, h);
                } else {
                    cr.set_source_rgba(0.694, 0.608, 0.808, 0.45);
                    v3::draw_round_rect(cr, x0, 3.0, x1 - x0, h - 6.0, 2.0);
                }
                let _ = cr.fill();
            }
        }
        "Chapters" => {
            for (start, end, chapter) in &state.chapters {
                let (x0, x1) = (x(*start), x(*end).max(x(*start) + 3.0));
                let (r, g, b, a) = match chapter {
                    ChapterState::Accepted => (1.0, 0.8, 0.0, 0.55),
                    ChapterState::Draft => (0.694, 0.608, 0.808, 0.32),
                    ChapterState::Stale => (0.725, 0.745, 0.769, 0.5),
                };
                v3::draw_round_rect(cr, x0, 2.0, x1 - x0, h - 4.0, 3.0);
                cr.set_source_rgba(r, g, b, a);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(r, g, b, 0.85);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }
        }
        _ => {
            for (start, end) in &state.coverage.speech {
                let (x0, x1) = (x(*start), x(*end).max(x(*start) + 2.0));
                v3::draw_round_rect(cr, x0, 4.0, x1 - x0, h - 8.0, 2.0);
                cr.set_source_rgba(0.627, 0.886, 0.918, 0.6);
                let _ = cr.fill();
            }
        }
    }
    if let Some(playhead) = state.playhead {
        cr.set_source_rgb(1.0, 0.8, 0.0);
        cr.rectangle(x(playhead) - 0.75, 0.0, 1.5, h);
        let _ = cr.fill();
    }
}

fn draw_ticks(cr: &gtk::cairo::Context, duration: f64, width: i32) {
    if duration <= 0.0 {
        return;
    }
    let w = f64::from(width);
    let step = ruler_step(duration);
    let ticks = (duration / step).floor() as usize;
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
    for index in 0..=ticks {
        let x = (index as f64 * step / duration * w).min(w - 1.0);
        cr.rectangle(x, 0.0, 1.0, 5.0);
    }
    let _ = cr.fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_steps_keep_eight_ticks_or_fewer() {
        assert_eq!(ruler_step(6.0), 1.0);
        assert_eq!(ruler_step(40.0), 5.0);
        assert_eq!(ruler_step(600.0), 120.0);
        assert_eq!(ruler_step(3.0 * 3600.0), 1800.0);
        assert_eq!(ruler_step(100.0 * 3600.0), 7200.0);
    }

    #[test]
    fn coverage_reads_examined_speech_and_people() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE catalog_coverage (id TEXT, file_id INTEGER, start_seconds REAL, end_seconds REAL, status TEXT);
             CREATE TABLE catalog_passages (id TEXT, file_id INTEGER, start_seconds REAL, end_seconds REAL, stale INTEGER);
             CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE catalog_observations (file_id INTEGER, person_id INTEGER, start_seconds REAL);
             INSERT INTO catalog_coverage VALUES ('a',1,0,0.1,'sampled'),('b',1,10,14,'verified'),('c',1,20,21,'incomplete'),('d',2,0,1,'sampled');
             INSERT INTO catalog_passages VALUES ('speech:1',1,30,60,0),('speech:2',1,60,90,1),('page:1',1,NULL,NULL,0);
             INSERT INTO persons VALUES (1,'Ada'),(2,NULL);
             INSERT INTO catalog_observations VALUES (1,1,12),(1,1,5),(1,2,NULL);",
        )
        .unwrap();
        let coverage = read_coverage(&conn, 1);
        assert_eq!(coverage.examined, vec![(0.0, 0.1), (10.0, 14.0)]);
        assert_eq!(coverage.speech, vec![(30.0, 60.0)]);
        assert_eq!(
            coverage.people,
            vec![
                PersonMoment { name: "Ada".into(), start: Some(5.0), reviewed: true },
                PersonMoment { name: "Unnamed person".into(), start: None, reviewed: false },
            ]
        );
    }

    #[test]
    fn chapters_are_accepted_draft_or_stale() {
        let mut chapter = CatalogChapter {
            id: "c".into(),
            file_id: 1,
            start_seconds: 0.0,
            end_seconds: 1.0,
            title: "t".into(),
            summary: String::new(),
            source_revision: String::new(),
            model_version: "timeline-chapter-suggestion-v1/x".into(),
            confidence: 0.6,
            user_edited: false,
            stale: false,
        };
        assert_eq!(chapter_state(&chapter), ChapterState::Draft);
        chapter.user_edited = true;
        assert_eq!(chapter_state(&chapter), ChapterState::Accepted);
        chapter.stale = true;
        assert_eq!(chapter_state(&chapter), ChapterState::Stale);
    }
}
