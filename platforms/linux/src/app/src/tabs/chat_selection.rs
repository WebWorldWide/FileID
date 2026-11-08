// Answers to a question scoped to selected files (macOS
// `SelectionResultsStrip`): matches, then the ones the model wasn't sure
// about, as tiles that can be chosen again for the next question, opened, or
// saved as a Best Takes group.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use fileid_engine::ipc::{CatalogEvent, CatalogHit, CatalogRequestPayload, CommandPayload};
use gtk::glib;
use gtk::prelude::*;

use super::chat_handoff::{self, ChatScope};
use super::workbench_state::{catalog_request, request_id};
use super::workbenches::{Engine, is_playable_media};
use crate::engine_client::{EngineState, texture_from_decoded};
use crate::v3;

const TILE_W: i32 = 132;
const TILE_H: i32 = 92;

pub(super) fn has_results(hits: &[CatalogHit]) -> bool {
    hits.iter().any(|hit| hit.selection_match.is_some())
}

fn kind(path: &str) -> &'static str {
    if is_playable_media(path) {
        "video"
    } else {
        "image"
    }
}

/// The chosen tiles, or every match when none are chosen (every file when there are no matches).
pub(super) fn target(hits: &[CatalogHit], chosen: &HashSet<i64>) -> Vec<i64> {
    if !chosen.is_empty() {
        return hits
            .iter()
            .map(|hit| hit.file_id)
            .filter(|id| chosen.contains(id))
            .collect();
    }
    let matches: Vec<i64> = hits
        .iter()
        .filter(|hit| hit.selection_match.as_deref() == Some("yes"))
        .map(|hit| hit.file_id)
        .collect();
    if matches.is_empty() {
        hits.iter().map(|hit| hit.file_id).collect()
    } else {
        matches
    }
}

pub(super) fn ask_label(hits: &[CatalogHit], chosen: usize) -> String {
    let matches = hits
        .iter()
        .filter(|hit| hit.selection_match.as_deref() == Some("yes"))
        .count();
    match (chosen, matches) {
        (0, 0) => format!("Ask about these {}", hits.len()),
        (0, 1) => "Ask about the 1 match".to_owned(),
        (0, n) => format!("Ask about the {n} matches"),
        (n, _) => format!("Ask about {n} selected"),
    }
}

struct Strip {
    engine: Engine,
    hits: Vec<CatalogHit>,
    chosen: RefCell<HashSet<i64>>,
    toggles: RefCell<Vec<gtk::ToggleButton>>,
    ask: gtk::Button,
    group: gtk::Button,
    all: gtk::Button,
    note: gtk::Label,
    question: String,
}

impl Strip {
    fn refresh(&self) {
        let chosen = self.chosen.borrow();
        self.ask.set_label(&ask_label(&self.hits, chosen.len()));
        let count = target(&self.hits, &chosen).len();
        self.group.set_sensitive(count >= 2);
        self.group.set_tooltip_text(Some(if count < 2 {
            "Choose at least two files"
        } else {
            "Save these as a group in Best Takes"
        }));
        let every = chosen.len() == self.hits.len();
        self.all
            .set_label(if every { "Clear" } else { "Select all" });
        self.all
            .update_property(&[gtk::accessible::Property::Label(if every {
                "Clear the selection"
            } else {
                "Select every file in this answer"
            })]);
    }

    fn make_group(&self) {
        let ids = target(&self.hits, &self.chosen.borrow());
        let title: String = self.question.trim().chars().take(80).collect();
        let mut request = catalog_request("saveEvent");
        request.event = Some(CatalogEvent {
            id: request_id(),
            title: title.clone(),
            goal: title,
            file_ids: ids.clone(),
            user_edited: true,
        });
        let wanted = request.request_id.clone();
        let rx = self.engine.borrow_mut().subscribe();
        if self
            .engine
            .borrow_mut()
            .send(CommandPayload::CatalogRequest(Box::new(
                CatalogRequestPayload { request },
            )))
            .is_err()
        {
            self.note
                .set_label("FileID is still getting ready. Try again in a moment.");
            return;
        }
        self.note.set_label("Saving the group…");
        let note = self.note.downgrade();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                if let EngineState::CatalogResponse(response) = event
                    && response.request_id == wanted
                {
                    if let Some(note) = note.upgrade() {
                        note.set_label(&if response.status == "ok" {
                            format!("Saved {} files as a group in Best Takes.", ids.len())
                        } else {
                            response
                                .message
                                .unwrap_or_else(|| "The group couldn't be saved. Try again.".into())
                        });
                    }
                    break;
                }
            }
        });
    }
}

/// `on_open` opens a hit by its index in `hits`.
pub(super) fn strip(
    engine: &Engine,
    hits: &[CatalogHit],
    question: &str,
    on_open: Rc<dyn Fn(usize)>,
) -> gtk::Box {
    let root = v3::vbox(10);
    root.add_css_class("v3-selection-results");
    let actions = v3::hbox(8);
    let ask = v3::secondary("Ask about these", false);
    ask.set_tooltip_text(Some("Ask the next question about only these files"));
    let group = v3::secondary("Make a group", false);
    let all = v3::ghost("Select all");
    actions.append(&ask);
    actions.append(&group);
    actions.append(&all);
    let note = v3::wrapped("", &["v3-meta"]);
    let strip = Rc::new(Strip {
        engine: engine.clone(),
        hits: hits.to_vec(),
        chosen: RefCell::new(HashSet::new()),
        toggles: RefCell::new(Vec::new()),
        ask: ask.clone(),
        group: group.clone(),
        all: all.clone(),
        note: note.clone(),
        question: question.to_owned(),
    });
    for (title, verdict, tone) in [
        ("Matches", "yes", "tone-gold"),
        ("Not sure", "unsure", "tone-ai"),
    ] {
        let items: Vec<(usize, &CatalogHit)> = hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.selection_match.as_deref() == Some(verdict))
            .collect();
        if items.is_empty() {
            continue;
        }
        let heading = v3::section(&format!("{title} · {}", items.len()));
        heading.add_css_class(tone);
        root.append(&heading);
        let row = v3::hbox(8);
        for (index, hit) in items {
            row.append(&tile(&strip, hit, index, on_open.clone()));
        }
        let scroll = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&row)
            .build();
        root.append(&scroll);
    }
    root.append(&actions);
    root.append(&note);
    let weak = Rc::downgrade(&strip);
    ask.connect_clicked(move |_| {
        if let Some(strip) = weak.upgrade() {
            let ids = target(&strip.hits, &strip.chosen.borrow());
            let kinds: Vec<&str> = strip
                .hits
                .iter()
                .filter(|hit| ids.contains(&hit.file_id))
                .map(|hit| kind(&hit.path))
                .collect();
            chat_handoff::set_scope(ChatScope::selected(&ids, &kinds));
        }
    });
    let weak = Rc::downgrade(&strip);
    group.connect_clicked(move |_| {
        if let Some(strip) = weak.upgrade() {
            strip.make_group();
        }
    });
    let weak = Rc::downgrade(&strip);
    all.connect_clicked(move |_| {
        if let Some(strip) = weak.upgrade() {
            let every = strip.chosen.borrow().len() == strip.hits.len();
            for toggle in strip.toggles.borrow().iter() {
                toggle.set_active(!every);
            }
        }
    });
    strip.refresh();
    root
}

fn tile(strip: &Rc<Strip>, hit: &CatalogHit, index: usize, on_open: Rc<dyn Fn(usize)>) -> gtk::Box {
    let column = v3::vbox(5);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .width_request(TILE_W)
        .height_request(TILE_H)
        .css_classes(["v3-thumb"])
        .build();
    let rx = if kind(&hit.path) == "video" {
        strip
            .engine
            .borrow()
            .request_video_thumbnail(hit.path.clone(), 256)
    } else {
        strip
            .engine
            .borrow()
            .request_scaled_thumbnail(hit.path.clone(), 256)
    };
    let weak = picture.downgrade();
    glib::MainContext::default().spawn_local(async move {
        if let Ok(Some(decoded)) = rx.recv().await
            && let Some(picture) = weak.upgrade()
        {
            picture.set_paintable(Some(&texture_from_decoded(decoded)));
        }
    });
    let matched = hit.selection_match.as_deref() == Some("yes");
    let badge = v3::label(
        if matched { "✓" } else { "?" },
        &[
            "v3-selection-badge",
            if matched { "tone-gold" } else { "tone-ai" },
        ],
    );
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Start);
    let overlay = gtk::Overlay::builder().child(&picture).build();
    overlay.add_overlay(&badge);
    let toggle = gtk::ToggleButton::builder()
        .child(&overlay)
        .has_frame(false)
        .css_classes(["v3-selection-tile"])
        .build();
    let name = v3::file_name(&hit.path);
    let spoken = format!(
        "{}: {name}. {}",
        if matched { "Match" } else { "Not sure" },
        hit.text
    );
    toggle.update_property(&[
        gtk::accessible::Property::Label(&spoken),
        gtk::accessible::Property::Description(
            "Toggles this file for the next question. Double-click opens it.",
        ),
    ]);
    toggle.set_tooltip_text(Some(&format!("{name} · {}", hit.text)));
    let id = hit.file_id;
    let weak = Rc::downgrade(strip);
    toggle.connect_toggled(move |toggle| {
        if let Some(strip) = weak.upgrade() {
            if toggle.is_active() {
                strip.chosen.borrow_mut().insert(id);
            } else {
                strip.chosen.borrow_mut().remove(&id);
            }
            v3::set_selected(toggle, toggle.is_active());
            strip.refresh();
        }
    });
    let double = gtk::GestureClick::new();
    double.connect_pressed(move |_, presses, _, _| {
        if presses == 2 {
            on_open(index);
        }
    });
    toggle.add_controller(double);
    strip.toggles.borrow_mut().push(toggle.clone());
    column.append(&toggle);
    let reason = v3::wrapped(&hit.text, &["v3-snippet"]);
    reason.set_lines(3);
    reason.set_ellipsize(gtk::pango::EllipsizeMode::End);
    reason.set_max_width_chars(18);
    reason.set_width_request(TILE_W);
    column.append(&reason);
    column
}

/// The scope chip above the composer, with a remove button.
pub(super) fn chip(scope: &ChatScope) -> gtk::Box {
    let chip = v3::hbox(6);
    chip.add_css_class("v3-scope-chip");
    chip.set_halign(gtk::Align::Start);
    let label = v3::label(&scope.label, &["v3-semibold", "tone-gold"]);
    chip.append(&label);
    let remove = v3::icon_button("window-close-symbolic", "Ask about the whole library again");
    remove.update_property(&[gtk::accessible::Property::Label(&format!(
        "Remove {} from the question",
        scope.label
    ))]);
    remove.connect_clicked(|_| chat_handoff::set_scope(None));
    chip.append(&remove);
    chip.update_property(&[gtk::accessible::Property::Label(&format!(
        "Question scope: {}",
        scope.label
    ))]);
    chip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: i64, verdict: &str) -> CatalogHit {
        CatalogHit {
            file_id: id,
            path: format!("/f/{id}.jpg"),
            kind: "selection".into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: None,
            page: None,
            selection_match: Some(verdict.into()),
        }
    }

    #[test]
    fn the_next_question_targets_chosen_tiles_then_matches_then_everything() {
        let hits = vec![hit(1, "yes"), hit(2, "unsure"), hit(3, "yes")];
        assert_eq!(target(&hits, &HashSet::new()), vec![1, 3]);
        assert_eq!(target(&hits, &[2, 3].into_iter().collect()), vec![2, 3]);
        assert_eq!(target(&[hit(4, "unsure")], &HashSet::new()), vec![4]);
        assert_eq!(ask_label(&hits, 0), "Ask about the 2 matches");
        assert_eq!(ask_label(&hits, 1), "Ask about 1 selected");
        assert_eq!(ask_label(&[hit(4, "unsure")], 0), "Ask about these 1");
        assert!(has_results(&hits));
        assert!(!has_results(&[CatalogHit {
            selection_match: None,
            ..hit(5, "yes")
        }]));
    }
}
