// Chat (DESIGN_V3_PORT "Chat"): the conversation column with LOOKING IN
// chips, role-railed messages, citation chips under the latest answer and the
// composer, beside a 300 px Sources pane with an inline player.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::moments::hit_kind;
use super::workbench_state::*;
use super::workbenches::{is_playable_media, Engine};
use crate::engine_client::EngineState;
use crate::shell::status::Tone;
use crate::v3;

const CONVERSATION: &str = "linux-default";
const MAX_CITATIONS: usize = 8;
pub const ASK_ACTION: &str = "ask";
const SUGGESTIONS: [&str; 3] = [
    "Photos from the beach last summer",
    "Where is the contract we signed?",
    "Clips where someone is speaking",
];

struct ChatUi {
    engine: Engine,
    state: Rc<RefCell<ChatState>>,
    status: gtk::Label,
    messages: gtk::Box,
    empty: gtk::Box,
    busy: gtk::Box,
    composer: gtk::TextView,
    placeholder: gtk::Label,
    send: gtk::Button,
    stop: gtk::Button,
    clear: gtk::Button,
    sources: gtk::Box,
    sources_count: gtk::Label,
    player_frame: gtk::Box,
    video: gtk::Video,
    active_source: Cell<Option<usize>>,
    hits: RefCell<Vec<CatalogHit>>,
    has_messages: Cell<bool>,
}

impl ChatUi {
    fn text(&self) -> String {
        let buffer = self.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn set_text(&self, text: &str) {
        self.composer.buffer().set_text(text);
        self.composer.grab_focus();
    }

    fn dispatch(&self, action: &str) {
        let text = self.text();
        if action == "send" && text.trim().is_empty() {
            return;
        }
        let request = ChatRequest {
            request_id: request_id(),
            conversation_id: CONVERSATION.into(),
            action: action.into(),
            text: (action == "send").then(|| text.chars().take(2000).collect()),
            use_model: Some(false),
            ..ChatRequest::default()
        };
        let dispatch = self.state.borrow_mut().begin(&request);
        match dispatch {
            Ok(true) => {
                if action == "send" {
                    self.composer.buffer().set_text("");
                    self.set_busy(true);
                }
                self.status.set_label("");
                send_request(&self.engine, &self.state, &self.status, request);
            }
            Ok(false) => self.status.set_label(
                "Clear waits until the answer being saved finishes. Nothing is deleted yet.",
            ),
            Err(reason) => self.status.set_label(reason),
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set_visible(busy);
        self.send.set_visible(!busy);
        self.stop.set_visible(busy);
    }

    fn render(self: &Rc<Self>, response: ChatResponse) {
        self.set_busy(false);
        if response.status != "ok" || !response.message.is_empty() {
            self.status.set_label(&response.message);
        }
        v3::clear(&self.messages);
        self.has_messages.set(!response.messages.is_empty());
        self.empty.set_visible(response.messages.is_empty());
        self.clear.set_sensitive(!response.messages.is_empty());
        self.clear.set_tooltip_text(Some(if response.messages.is_empty() {
            "Nothing to clear yet"
        } else {
            "Start a new conversation. This one is deleted."
        }));
        let last_answer = response.messages.iter().rposition(|m| m.role != "user");
        for (index, message) in response.messages.iter().enumerate() {
            self.messages.append(&message_row(message));
            if Some(index) == last_answer && !response.hits.is_empty() {
                self.messages.append(&self.citations(&response.hits));
            }
        }
        self.render_sources(response.hits);
    }

    fn citations(self: &Rc<Self>, hits: &[CatalogHit]) -> gtk::FlowBox {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(4)
            .css_classes(["v3-citations"])
            .build();
        for (index, hit) in hits.iter().take(MAX_CITATIONS).enumerate() {
            let content = v3::hbox(4);
            let number = v3::mono(&format!("[{}]", index + 1), "s11");
            number.add_css_class("tone-gold");
            number.add_css_class("v3-semibold");
            content.append(&number);
            let name = v3::mono(&v3::file_name(&hit.path), "s11");
            name.add_css_class("tone-text2");
            name.set_max_width_chars(22);
            content.append(&name);
            let meta = citation_meta(hit);
            let meta_label = v3::mono(&format!("· {meta}"), "s11");
            meta_label.add_css_class("tone-muted");
            content.append(&meta_label);
            let chip = gtk::Button::builder()
                .child(&content)
                .css_classes(["v3-cite"])
                .build();
            chip.update_property(&[
                gtk::accessible::Property::Label(&format!(
                    "Source {}: {}, {meta}",
                    index + 1,
                    v3::file_name(&hit.path)
                )),
                gtk::accessible::Property::Description("Opens the file"),
            ]);
            chip.set_tooltip_text(Some("Opens the file"));
            let weak = Rc::downgrade(self);
            chip.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_source(index);
                }
            });
            flow.append(&chip);
        }
        flow
    }

    fn render_sources(self: &Rc<Self>, hits: Vec<CatalogHit>) {
        v3::clear(&self.sources);
        self.sources_count.set_text(&hits.len().to_string());
        if hits.is_empty() {
            let empty = v3::wrapped("Files the answer draws on appear here.", &["v3-meta"]);
            empty.set_margin_start(14);
            empty.set_margin_end(14);
            self.sources.append(&empty);
        }
        for (index, hit) in hits.iter().enumerate() {
            let row = v3::hbox(8);
            let number = v3::mono(&format!("[{}]", index + 1), "s115");
            number.add_css_class("tone-gold");
            number.add_css_class("v3-semibold");
            number.set_size_request(30, -1);
            number.set_valign(gtk::Align::Start);
            row.append(&number);
            let text = v3::vbox(2);
            let name = v3::mono(&v3::file_name(&hit.path), "s12");
            name.add_css_class("v3-medium");
            text.append(&name);
            let (kind, sampled) = hit_kind(hit);
            let meta = v3::label(&citation_meta(hit), &["v3-meta-115"]);
            if sampled {
                meta.add_css_class("tone-ai");
            }
            meta.set_tooltip_text(Some(kind));
            text.append(&meta);
            if !hit.text.trim().is_empty() {
                let snippet = v3::wrapped(hit.text.trim(), &["v3-snippet"]);
                snippet.set_lines(3);
                snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
                text.append(&snippet);
            }
            row.append(&text);
            let button = v3::rail_row(&row);
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_source(index);
                }
            });
            let menu_click = gtk::GestureClick::builder().button(3).build();
            let path = hit.path.clone();
            let anchor = button.downgrade();
            menu_click.connect_pressed(move |_, _, _, _| {
                if let Some(anchor) = anchor.upgrade() {
                    show_source_menu(&anchor, &path);
                }
            });
            button.add_controller(menu_click);
            self.sources.append(&button);
        }
        self.hits.replace(hits);
        self.active_source.set(None);
    }

    /// Playable media plays inline in the Sources pane; everything else
    /// opens in the default app.
    fn open_source(&self, index: usize) {
        let Some(hit) = self.hits.borrow().get(index).cloned() else {
            return;
        };
        self.active_source.set(Some(index));
        let mut child = self.sources.first_child();
        let mut position = 0;
        while let Some(widget) = child {
            v3::set_selected(&widget, position == index);
            position += 1;
            child = widget.next_sibling();
        }
        if !std::path::Path::new(&hit.path).is_file() {
            self.status.set_label("That file is offline or has moved.");
            return;
        }
        if hit.kind != "page" && is_playable_media(&hit.path) {
            let media = gtk::MediaFile::for_filename(&hit.path);
            if let Some(seconds) = hit.start_seconds.filter(|s| s.is_finite() && *s > 0.0) {
                media.connect_prepared_notify(move |stream| {
                    if stream.is_prepared() && stream.is_seekable() {
                        stream.seek((seconds * 1_000_000.0) as i64);
                    }
                });
            }
            self.video.set_media_stream(Some(&media));
            self.player_frame.set_visible(true);
            media.play();
        } else {
            v3::open_file(&self.sources, &hit.path);
        }
    }
}

fn citation_meta(hit: &CatalogHit) -> String {
    match (hit.start_seconds, hit.page) {
        (Some(start), _) => v3::timecode(start),
        (None, Some(page)) => format!("page {page}"),
        _ => hit_kind(hit).0.to_lowercase(),
    }
}

fn message_row(message: &ChatMessage) -> gtk::Box {
    let you = message.role == "user";
    let row = v3::vbox(4);
    row.add_css_class("v3-message");
    row.add_css_class(if you { "you" } else { "model" });
    let role = v3::label(
        if you { "You" } else { "FileID" },
        &["v3-role", if you { "tone-gold" } else { "tone-ai" }],
    );
    row.append(&role);
    let body = v3::wrapped(&message.text, &["v3-message-body"]);
    body.set_selectable(true);
    row.append(&body);
    row
}

fn show_source_menu(anchor: &gtk::Button, path: &str) {
    let popover = gtk::Popover::builder()
        .css_classes(["fileid-popover"])
        .build();
    let reveal = v3::ghost("Show in Files");
    let path = path.to_owned();
    let weak = popover.downgrade();
    reveal.connect_clicked(move |button| {
        v3::show_in_files(button, &path);
        if let Some(popover) = weak.upgrade() {
            popover.popdown();
        }
    });
    popover.set_child(Some(&reveal));
    popover.set_parent(anchor);
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

fn send_request(
    engine: &Engine,
    state: &Rc<RefCell<ChatState>>,
    status: &gtk::Label,
    request: ChatRequest,
) {
    if engine
        .borrow_mut()
        .send(CommandPayload::ChatRequest(ChatRequestPayload { request }))
        .is_err()
    {
        state.borrow_mut().interrupted();
        status.set_label(
            "FileID is still getting ready. Try again in a moment. Saved answers reload once it is ready.",
        );
    }
}

pub fn chat(engine: Engine) -> gtk::Widget {
    build(engine, None)
}

/// Chat beside the current page (DESIGN_V3_PORT "Chat", inspector variant):
/// 380 px, three chips, a close button (Esc) and the sources folded into a
/// "SOURCES · N" row whose chevron turns on the standard spring.
pub fn inspector(engine: Engine, on_close: Rc<dyn Fn()>) -> gtk::Widget {
    build(engine, Some(on_close))
}

fn build(engine: Engine, inspector: Option<Rc<dyn Fn()>>) -> gtk::Widget {
    let compact = inspector.is_some();
    // ── Conversation column ────────────────────────────────────────────────
    let column = v3::vbox(0);
    column.set_hexpand(true);
    column.set_vexpand(true);
    let header = v3::hbox(12);
    header.add_css_class(if compact { "v3-inspector-header" } else { "v3-chat-header" });
    let looking = v3::vbox(6);
    looking.set_hexpand(true);
    looking.append(&v3::section("Looking in"));
    let chips = v3::hbox(6);
    let (names, spoken): (&[&str], &str) = if compact {
        (&["Names", "Tags", "Descriptions"], "Looking in names, tags and descriptions")
    } else {
        (
            &["Names", "Tags", "Descriptions", "Chapters", "Text in files"],
            "Looking in names, tags, descriptions, chapters and text in files",
        )
    };
    for chip in names {
        chips.append(&v3::label(chip, &["v3-chip"]));
    }
    chips.update_property(&[gtk::accessible::Property::Label(spoken)]);
    looking.append(&chips);
    header.append(&looking);
    let clear = v3::ghost("Clear");
    clear.set_tooltip_text(Some("Nothing to clear yet"));
    clear.set_sensitive(false);
    clear.set_valign(gtk::Align::Center);
    header.append(&clear);
    if let Some(on_close) = inspector.clone() {
        let close = v3::icon_button("window-close-symbolic", "Close chat (Esc)");
        close.set_valign(gtk::Align::Center);
        close.connect_clicked(move |_| on_close());
        header.append(&close);
    }
    column.append(&header);

    let messages = v3::vbox(20);
    let empty = v3::vbox(10);
    empty.append(&v3::label("Find the moment.", &["v3-serif-34"]));
    empty.append(&v3::wrapped(
        "Ask about your files in plain words. Answers cite the files they come from; nothing leaves this computer.",
        &["v3-body", "tone-text2"],
    ));
    let suggestions = v3::vbox(6);
    suggestions.set_halign(gtk::Align::Start);
    empty.append(&suggestions);
    let busy = v3::hbox(8);
    let busy_bar = v3::busy_bar(60);
    busy_bar.set_visible(true);
    busy.append(&busy_bar);
    busy.append(&v3::label(
        "Searching your library…",
        &["v3-meta", "tone-ai"],
    ));
    busy.set_visible(false);
    let thread = v3::vbox(20);
    thread.add_css_class(if compact { "v3-inspector-pad" } else { "v3-page-pad" });
    thread.append(&empty);
    thread.append(&messages);
    thread.append(&busy);
    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(760)
        .child(&thread)
        .build();
    let scroll = v3::scroller(&clamp);
    column.append(&scroll);

    let composer_box = v3::vbox(8);
    composer_box.add_css_class("v3-composer-box");
    if compact {
        composer_box.add_css_class("inspector");
    }
    let input_row = v3::hbox(8);
    let composer = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .css_classes(["v3-composer"])
        .build();
    composer.update_property(&[gtk::accessible::Property::Label("Ask about your files")]);
    let placeholder = v3::label("Ask about your files", &["v3-body", "tone-muted"]);
    placeholder.set_can_target(false);
    placeholder.set_valign(gtk::Align::Start);
    let composer_overlay = gtk::Overlay::builder().child(&composer).build();
    composer_overlay.add_overlay(&placeholder);
    let composer_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(5 * 20)
        .child(&composer_overlay)
        .hexpand(true)
        .build();
    let composer_frame = v3::vbox(0);
    composer_frame.add_css_class("v3-composer-frame");
    composer_frame.set_hexpand(true);
    composer_frame.append(&composer_scroll);
    input_row.append(&composer_frame);
    let send = v3::primary("Send", false);
    send.set_valign(gtk::Align::End);
    send.set_tooltip_text(Some("Type a question first"));
    let stop = v3::secondary("Stop", true);
    stop.set_valign(gtk::Align::End);
    stop.set_tooltip_text(Some("Stop (Ctrl+.)"));
    stop.set_visible(false);
    input_row.append(&send);
    input_row.append(&stop);
    composer_box.append(&input_row);
    let options = v3::hbox(10);
    let ai = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    ai.update_property(&[gtk::accessible::Property::Label("Summarize with local AI")]);
    let ai_row = v3::hbox(8);
    ai_row.add_css_class("v3-toggle-row");
    ai_row.append(&ai);
    ai_row.append(&v3::label("Summarize with local AI", &["v3-meta"]));
    ai_row.set_tooltip_text(Some(
        "Needs a local llama.cpp runtime, which Linux packages don't include yet. Answers list matching files only.",
    ));
    options.append(&ai_row);
    let model_chip = v3::accent("Chat model: none", Tone::Ai, false);
    model_chip.set_sensitive(false);
    model_chip.set_tooltip_text(Some("Turn on Summarize with local AI to choose a model"));
    model_chip.update_property(&[gtk::accessible::Property::Label("Chat model: none")]);
    options.append(&model_chip);
    composer_box.append(&options);
    let status = v3::wrapped("", &["v3-meta"]);
    composer_box.append(&status);
    column.append(&composer_box);

    // ── Sources pane ───────────────────────────────────────────────────────
    let pane = v3::vbox(0);
    pane.add_css_class("v3-pane");
    let sources_count = v3::meta("0");
    sources_count.add_css_class("numeric");
    let pane_header = v3::hbox(8);
    let pane_title = v3::section(if compact { "Sources ·" } else { "Sources" });
    if compact {
        pane_header.append(&pane_title);
        pane_header.append(&sources_count);
    } else {
        pane.set_size_request(300, -1);
        pane_header.add_css_class("v3-sources-header");
        pane_title.set_hexpand(true);
        pane_header.append(&pane_title);
        pane_header.append(&sources_count);
        pane.append(&pane_header);
    }
    let video = gtk::Video::builder()
        .autoplay(false)
        .css_classes(["v3-black"])
        .build();
    let player_frame = v3::vbox(0);
    player_frame.add_css_class("v3-panel-pad");
    player_frame.append(
        &gtk::AspectFrame::builder()
            .ratio(16.0 / 9.0)
            .obey_child(false)
            .child(&video)
            .build(),
    );
    player_frame.set_visible(false);
    pane.append(&player_frame);
    let sources = v3::vbox(2);
    let sources_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(!compact)
        .child(&sources)
        .build();
    pane.append(&sources_scroll);

    let root = if compact {
        let root = v3::vbox(0);
        root.add_css_class("v3-inspector");
        root.set_size_request(380, -1);
        root.append(&column);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sources_scroll.set_max_content_height(200);
        sources_scroll.set_propagate_natural_height(true);
        root.append(&collapsible_sources(&pane_header, &pane));
        root
    } else {
        let root = v3::hbox(0);
        root.append(&column);
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        root.append(&pane);
        root
    };

    let ui = Rc::new(ChatUi {
        engine: engine.clone(),
        state: Rc::new(RefCell::new(ChatState::default())),
        status,
        messages,
        empty,
        busy,
        composer,
        placeholder,
        send,
        stop,
        clear,
        sources,
        sources_count,
        player_frame,
        video,
        active_source: Cell::new(None),
        hits: RefCell::new(Vec::new()),
        has_messages: Cell::new(false),
    });
    ui.render_sources(Vec::new());
    for text in SUGGESTIONS {
        let content = v3::hbox(8);
        let caption = v3::label(text, &[]);
        caption.set_hexpand(true);
        content.append(&caption);
        content.append(&v3::label("↖", &["tone-muted"]));
        let suggestion = gtk::Button::builder()
            .child(&content)
            .css_classes(["v3-suggestion"])
            .build();
        if !compact {
            suggestion.set_size_request(460, -1);
        }
        let weak = Rc::downgrade(&ui);
        suggestion.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.set_text(text);
            }
        });
        suggestions.append(&suggestion);
    }
    wire(&ui, &root, inspector);
    root.upcast()
}

/// The inspector's 34 px "SOURCES · N" row: a chevron that turns 90° on the
/// standard spring and a revealer holding the player and the list.
fn collapsible_sources(header: &gtk::Box, pane: &gtk::Box) -> gtk::Box {
    let section = v3::vbox(0);
    let angle = Rc::new(Cell::new(0.0_f64));
    let chevron = gtk::DrawingArea::builder()
        .content_width(12)
        .content_height(12)
        .valign(gtk::Align::Center)
        .build();
    let draw_angle = angle.clone();
    chevron.set_draw_func(move |_, cr, w, h| {
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        cr.translate(cx, cy);
        cr.rotate(draw_angle.get().to_radians());
        cr.move_to(-2.0, -4.0);
        cr.line_to(2.0, 0.0);
        cr.line_to(-2.0, 4.0);
        cr.set_source_rgb(0.651, 0.675, 0.702);
        cr.set_line_width(1.5);
        let _ = cr.stroke();
    });
    let row = v3::hbox(8);
    row.append(&chevron);
    row.append(header);
    let toggle = gtk::Button::builder()
        .child(&row)
        .css_classes(["v3-sources-toggle"])
        .build();
    toggle.update_property(&[gtk::accessible::Property::Label("Sources")]);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    section.append(&toggle);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(if crate::spring::animations_enabled() { 220 } else { 0 })
        .child(pane)
        .build();
    section.append(&revealer);
    let animation: Rc<RefCell<Option<adw::SpringAnimation>>> = Rc::new(RefCell::new(None));
    toggle.connect_clicked(move |toggle| {
        let open = !revealer.reveals_child();
        revealer.set_reveal_child(open);
        toggle.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        if let Some(previous) = animation.take() {
            previous.pause();
        }
        let target = angle.clone();
        let area = chevron.clone();
        let next = crate::spring::animate(&chevron, angle.get(), if open { 90.0 } else { 0.0 }, move |value| {
            target.set(value);
            area.queue_draw();
        });
        animation.replace(Some(next));
    });
    section
}

fn wire(ui: &Rc<ChatUi>, root: &gtk::Box, inspector: Option<Rc<dyn Fn()>>) {
    if let Some(on_close) = inspector {
        let escape = gtk::EventControllerKey::new();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                on_close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        root.add_controller(escape);
    }
    // The page and the inspector share one conversation; whichever appears
    // reloads it so both show what the other added.
    let weak = Rc::downgrade(ui);
    root.connect_map(move |_| {
        if let Some(ui) = weak.upgrade() {
            if ui.engine.borrow().is_ready() && !ui.stop.is_visible() {
                ui.dispatch("history");
            }
        }
    });
    let weak = Rc::downgrade(ui);
    ui.composer.buffer().connect_changed(move |buffer| {
        if let Some(ui) = weak.upgrade() {
            ui.placeholder.set_visible(buffer.char_count() == 0);
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false);
            ui.send.set_tooltip_text(Some(if text.trim().is_empty() {
                "Type a question first"
            } else if text.chars().count() > 2000 {
                "Shorten the question to 2,000 characters"
            } else {
                "Send (Return)"
            }));
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let plain_return = matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
            && !modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
        if plain_return {
            ui.dispatch("send");
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    ui.composer.add_controller(keys);
    for (button, action) in [
        (&ui.send, "send"),
        (&ui.stop, "cancel"),
        (&ui.clear, "clear"),
    ] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.dispatch(action);
                if action == "cancel" {
                    ui.set_busy(false);
                }
            }
        });
    }
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Managed);
    let weak = Rc::downgrade(ui);
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>period"),
        Some(gtk::CallbackAction::new(move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| ui.stop.is_visible()) {
                ui.dispatch("cancel");
                ui.set_busy(false);
            }
            glib::Propagation::Stop
        })),
    ));
    root.add_controller(shortcuts);

    // The shell's "Ask Chat" hands the question over through chat.ask.
    let group = gio::SimpleActionGroup::new();
    let ask = gio::SimpleAction::new(ASK_ACTION, Some(glib::VariantTy::STRING));
    let weak = Rc::downgrade(ui);
    ask.connect_activate(move |_, parameter| {
        if let (Some(ui), Some(text)) = (weak.upgrade(), parameter.and_then(|value| value.str())) {
            ui.set_text(text);
        }
    });
    group.add_action(&ask);
    root.insert_action_group("chat", Some(&group));

    let rx = ui.engine.borrow_mut().subscribe();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EngineState::Ready => ui.dispatch("history"),
                EngineState::ChatResponse(response) => {
                    let deferred = ui.state.borrow_mut().finish(&response);
                    if let Some(request) = deferred {
                        send_request(&ui.engine, &ui.state, &ui.status, request);
                    }
                    let current = chat_matches(
                        &mut ui.state.borrow_mut().display,
                        &response.request_id,
                        &response.conversation_id,
                        CONVERSATION,
                        &response.status,
                    );
                    if current {
                        ui.render(response);
                    }
                }
                EngineState::Failed(error) => {
                    ui.state.borrow_mut().interrupted();
                    ui.set_busy(false);
                    ui.status.set_label(&error);
                }
                EngineState::Exited => {
                    ui.state.borrow_mut().interrupted();
                    ui.set_busy(false);
                    ui.status
                        .set_label("FileID's background helper stopped, so the answer was interrupted. Try again in a moment.");
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_meta_prefers_time_then_page() {
        let mut hit = CatalogHit {
            file_id: 1,
            path: "/a/b.mp4".into(),
            kind: "passage".into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: Some(75.0),
            page: None,
        };
        assert_eq!(citation_meta(&hit), "1:15");
        hit.start_seconds = None;
        hit.page = Some(3);
        assert_eq!(citation_meta(&hit), "page 3");
        hit.page = None;
        assert_eq!(citation_meta(&hit), "text in image");
    }
}
