// File Tools (DESIGN_V3_PORT "File Tools"): 01 · Pick files (kind, file
// checklist), 02 · Output (format, size, frame, folder) and a 380 px
// 03 · Preview pane with the gold-framed preview, outputs and actions.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::workbench_state::*;
use super::workbenches::{Engine, LINUX_TOOL_UNDO_SAFETY_ACCEPTED, TOOL_UNDO_UNAVAILABLE_REASON};
use crate::engine_client::{
    read_file_row, texture_from_decoded, DecodedImage, EngineState, FileRow, QueryChannel, QuerySpec,
    ThumbReply,
};
use crate::shell::status::Tone;
use crate::spring::{animate_with, Spring};
use crate::v3;

const KINDS: [&str; 3] = ["photo", "video", "chapters"];
const PHOTO_FORMATS: [(&str, &str); 3] = [("PNG", "png"), ("JPEG", "jpeg"), ("TIFF", "tiff")];
const CHAPTER_FORMATS: [(&str, &str); 2] = [("WebVTT", "vtt"), ("JSON", "json")];
const FRAMES: [(&str, Option<&str>, f64); 5] = [
    ("Original", None, 16.0 / 9.0),
    ("16:9", Some("16:9"), 16.0 / 9.0),
    ("9:16", Some("9:16"), 9.0 / 16.0),
    ("1:1", Some("1:1"), 1.0),
    ("4:5", Some("4:5"), 4.0 / 5.0),
];
const RESOLUTIONS: [(&str, u32); 2] = [("720p", 1280), ("1080p", 1920)];
const MAX_FILES: usize = 100;
pub(super) const VIDEO_TOOLS: [(&str, &str); 6] = [
    ("export", "Export"),
    ("trim", "Trim"),
    ("remux", "Remux"),
    ("audio", "Audio"),
    ("proxy", "Proxy"),
    ("rotate", "Rotate"),
];
const CONTAINERS: [(&str, &str); 2] = [("MP4", "mp4"), ("MOV", "mov")];
const PROXY_SIDES: [(&str, u32); 2] = [("640 px", 640), ("960 px", 960)];
const ROTATIONS: [(&str, u32); 3] = [("90° right", 90), ("180°", 180), ("90° left", 270)];

/// Export, plus each common-media tool the engine reports available.
pub(super) fn available_video_tools(capabilities: &[ToolCapability]) -> Vec<(&'static str, &'static str)> {
    VIDEO_TOOLS
        .iter()
        .copied()
        .filter(|(id, _)| *id == "export" || capabilities.iter().any(|c| c.id == *id && c.available))
        .collect()
}

pub(super) fn source_container(path: &str) -> Option<usize> {
    let extension = std::path::Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" => Some(0),
        "mov" => Some(1),
        _ => None,
    }
}

/// `rgba` turned `quarter_turns` × 90° clockwise.
pub(super) fn turn_rgba(rgba: &[u8], width: usize, height: usize, quarter_turns: u32) -> (Vec<u8>, usize, usize) {
    let turns = quarter_turns % 4;
    let (out_width, out_height) = if turns % 2 == 1 { (height, width) } else { (width, height) };
    let mut out = vec![0; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let (nx, ny) = match turns {
                1 => (height - 1 - y, x),
                2 => (width - 1 - x, height - 1 - y),
                3 => (y, width - 1 - x),
                _ => (x, y),
            };
            let from = (y * width + x) * 4;
            let to = (ny * out_width + nx) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (out, out_width, out_height)
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct RecipeChoice {
    pub kind: usize,
    pub format: usize,
    pub longest_side: u32,
    pub resolution: usize,
    pub frame: usize,
    pub enlarge: bool,
    pub tool: &'static str,
    pub trim_start: f64,
    pub trim_end: f64,
    pub trim_exact: bool,
    pub container: usize,
    pub proxy: usize,
    pub rotation: usize,
}

impl RecipeChoice {
    fn media_tool(&self) -> Option<&'static str> {
        (KINDS[self.kind.min(2)] == "video" && self.tool != "export").then_some(self.tool)
    }

    fn media_format(&self) -> &'static str {
        match self.tool {
            "audio" => "m4a",
            "proxy" => "mp4",
            "trim" if self.trim_exact => "mp4",
            _ => CONTAINERS[self.container.min(1)].1,
        }
    }

    /// Container indices to grey out: the one a remux source already has,
    /// and MOV for an exact trim (MP4 only).
    pub fn disabled_containers(&self, sources: &[&str]) -> Vec<usize> {
        let mut disabled: Vec<usize> = match self.tool {
            "remux" => sources.iter().filter_map(|path| source_container(path)).collect(),
            "trim" if self.trim_exact => vec![1],
            _ => vec![],
        };
        disabled.sort_unstable();
        disabled.dedup();
        disabled
    }

    pub fn settle_container(&mut self, sources: &[&str]) {
        let disabled = self.disabled_containers(sources);
        if disabled.contains(&self.container)
            && let Some(allowed) = (0..CONTAINERS.len()).find(|index| !disabled.contains(index))
        {
            self.container = allowed;
        }
    }

    pub fn adopt_tool(&mut self, tool: &'static str, sources: &[&str]) {
        self.tool = tool;
        if matches!(tool, "trim" | "rotate")
            && let Some(container) = sources.first().and_then(|path| source_container(path))
        {
            self.container = container;
        }
        self.settle_container(sources);
    }

    /// Clockwise quarter turns the preview shows.
    pub fn preview_turns(&self) -> u32 {
        if self.media_tool() == Some("rotate") {
            ROTATIONS[self.rotation.min(2)].1 / 90
        } else {
            0
        }
    }

    fn media_recipe(&self, tool: &str) -> ToolRecipe {
        let mut recipe = ToolRecipe {
            kind: tool.into(),
            format: self.media_format().into(),
            max_dimension: 1920,
            allow_upscale: None,
            video_aspect_ratio: None,
            video_framing: None,
            video_stabilization: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            trim_mode: None,
            rotation_degrees: None,
        };
        match tool {
            "trim" => {
                recipe.trim_start_seconds = Some(self.trim_start);
                recipe.trim_end_seconds = Some(self.trim_end);
                recipe.trim_mode = Some(if self.trim_exact { "exact" } else { "keyframe" }.into());
            }
            "proxy" => recipe.max_dimension = PROXY_SIDES[self.proxy.min(1)].1,
            "rotate" => recipe.rotation_degrees = Some(ROTATIONS[self.rotation.min(2)].1),
            _ => {}
        }
        recipe
    }

    pub fn recipe(&self) -> ToolRecipe {
        if let Some(tool) = self.media_tool() {
            return self.media_recipe(tool);
        }
        let kind = KINDS[self.kind.min(2)];
        let (format, max_dimension) = match kind {
            "photo" => (PHOTO_FORMATS[self.format.min(2)].1, self.longest_side),
            "video" => ("mp4", RESOLUTIONS[self.resolution.min(1)].1),
            _ => (CHAPTER_FORMATS[self.format.min(1)].1, 2048),
        };
        ToolRecipe {
            kind: kind.into(),
            format: format.into(),
            max_dimension,
            allow_upscale: (kind == "photo" && self.enlarge).then_some(true),
            video_aspect_ratio: if kind == "video" {
                FRAMES[self.frame.min(4)].1.map(str::to_owned)
            } else {
                None
            },
            video_framing: None,
            video_stabilization: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            trim_mode: None,
            rotation_degrees: None,
        }
    }

    pub fn summary(&self, files: usize) -> String {
        let recipe = self.recipe();
        let files = format!("{files} file{}", if files == 1 { "" } else { "s" });
        if let Some(tool) = self.media_tool() {
            let label = VIDEO_TOOLS.iter().find(|(id, _)| *id == tool).map_or(tool, |(_, label)| label);
            let mut parts = vec![files, label.to_owned(), recipe.format.to_uppercase()];
            match tool {
                "trim" => {
                    parts.push(format!("{:.2}–{:.2} s", self.trim_start, self.trim_end));
                    parts.push(if self.trim_exact { "Exact" } else { "Keyframe" }.to_owned());
                }
                "proxy" => parts.push(PROXY_SIDES[self.proxy.min(1)].0.to_owned()),
                "rotate" => parts.push(ROTATIONS[self.rotation.min(2)].0.to_owned()),
                _ => {}
            }
            return parts.join(" · ");
        }
        match recipe.kind.as_str() {
            "photo" => format!(
                "{files} · {} · ≤ {} px",
                recipe.format.to_uppercase(),
                recipe.max_dimension
            ),
            "video" => format!(
                "{files} · MP4 · H.264 · {} · {}",
                RESOLUTIONS[self.resolution.min(1)].0,
                FRAMES[self.frame.min(4)].0
            ),
            _ => format!("{files} · {}", CHAPTER_FORMATS[self.format.min(1)].0),
        }
    }

    pub fn frame_ratio(&self, photo_aspect: Option<f64>) -> f64 {
        if self.media_tool().is_some() {
            let ratio = photo_aspect.filter(|a| a.is_finite() && *a > 0.0).unwrap_or(16.0 / 9.0);
            return if self.preview_turns() % 2 == 1 { 1.0 / ratio } else { ratio };
        }
        match KINDS[self.kind.min(2)] {
            "photo" => photo_aspect
                .filter(|a| a.is_finite() && *a > 0.0)
                .unwrap_or(4.0 / 3.0),
            "video" => FRAMES[self.frame.min(4)].2,
            _ => 16.0 / 9.0,
        }
    }

    /// Photos list images, video lists videos, chapters any file.
    fn file_kind(&self) -> Option<&'static str> {
        match self.kind {
            0 => Some("image"),
            1 => Some("video"),
            _ => None,
        }
    }
}

pub(super) struct ToolsUi {
    pub(super) engine: Engine,
    pub(super) state: RefCell<ExportState>,
    pub(super) status: gtk::Label,
    pub(super) outputs: gtk::Box,
    pub(super) execute: gtk::Button,
    pub(super) undo: gtk::Button,
    pub(super) preview: gtk::Button,
    choice: RefCell<RecipeChoice>,
    kind: Rc<v3::Segmented>,
    photo_format: Rc<v3::Segmented>,
    chapter_format: Rc<v3::Segmented>,
    resolution: Rc<v3::Segmented>,
    longest_side: gtk::SpinButton,
    enlarge: gtk::Switch,
    frames: Vec<gtk::Button>,
    photo_rows: gtk::Box,
    video_rows: gtk::Box,
    format_stack: gtk::Stack,
    query: gtk::Entry,
    files: gtk::Box,
    files_count: gtk::Label,
    rows: RefCell<Vec<(FileRow, gtk::CheckButton)>>,
    selected: RefCell<Vec<i64>>,
    destination: RefCell<Option<String>>,
    destination_label: gtk::Label,
    frame: gtk::AspectFrame,
    frame_ratio: Rc<Cell<f64>>,
    frame_animation: RefCell<Option<adw::SpringAnimation>>,
    picture: gtk::Picture,
    photo_aspect: Cell<Option<f64>>,
    summary: gtk::Label,
    capability: gtk::Label,
    busy: gtk::ProgressBar,
    cancel: gtk::Button,
    running: RefCell<Option<String>>,
    lookup: Cell<u64>,
    loading: Cell<bool>,
    handoff: RefCell<Option<Rc<dyn Fn(Vec<i64>)>>>,
    tool_row: gtk::Box,
    tool_slot: gtk::Box,
    tool_picker: RefCell<Option<Rc<v3::Segmented>>>,
    media_format: Rc<v3::Segmented>,
    media_rows: gtk::Box,
    trim_rows: gtk::Box,
    trim_start: gtk::SpinButton,
    trim_end: gtk::SpinButton,
    trim_cut: Rc<v3::Segmented>,
    proxy_row: gtk::Box,
    proxy_size: Rc<v3::Segmented>,
    rotate_row: gtk::Box,
    rotation: Rc<v3::Segmented>,
    media_note: gtk::Label,
    preview_stack: gtk::Stack,
    video: gtk::Video,
    thumbnail: RefCell<Option<DecodedImage>>,
    player_path: RefCell<Option<String>>,
}

/// The export kind that fits every handed file, if one does.
pub(super) fn kind_for(kinds: &[&str]) -> Option<usize> {
    if kinds.is_empty() {
        None
    } else if kinds.iter().all(|kind| *kind == "image") {
        Some(0)
    } else if kinds.iter().all(|kind| *kind == "video") {
        Some(1)
    } else {
        None
    }
}

pub(super) fn handoff_status(chosen: usize, asked: usize) -> String {
    if asked > chosen {
        format!(
            "Chat chose {asked} files; the first {chosen} are ticked. Up to {MAX_FILES} files per export."
        )
    } else {
        format!(
            "Chat chose {chosen} {}. Pick a format, then Preview.",
            if chosen == 1 { "file" } else { "files" }
        )
    }
}

impl ToolsUi {
    fn begin_lookup(&self) -> u64 {
        let lookup = self.lookup.get().wrapping_add(1);
        self.lookup.set(lookup);
        self.loading.set(true);
        lookup
    }

    fn end_lookup(&self, lookup: u64) -> bool {
        let current = self.lookup.get() == lookup;
        if current {
            self.loading.set(false);
        }
        current
    }

    /// Chat's export card: tick the files it chose, up to the export limit.
    fn receive(self: &Rc<Self>, ids: Vec<i64>) {
        let asked = ids.len();
        let mut ids: Vec<i64> = ids.into_iter().filter(|id| *id > 0).collect();
        ids.dedup();
        ids.truncate(MAX_FILES);
        let lookup = self.begin_lookup();
        self.query.set_text("");
        self.status.set_label("Loading the files Chat chose…");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let mut rows = Vec::new();
            for id in ids {
                if let Ok(Some(row)) = read_file_row(id).recv().await {
                    rows.push(row);
                }
            }
            let Some(ui) = weak.upgrade() else { return };
            if !ui.end_lookup(lookup) {
                return;
            }
            let kinds: Vec<&str> = rows.iter().map(|row| row.kind.as_str()).collect();
            if let Some(kind) = kind_for(&kinds).filter(|kind| *kind != ui.choice.borrow().kind) {
                ui.kind.select(kind, false);
                ui.choice.borrow_mut().kind = kind;
                ui.sync_rows();
                ui.animate_frame();
            }
            ui.selected.replace(rows.iter().map(|row| row.id).collect());
            ui.status.set_label(&handoff_status(rows.len(), asked));
            ui.render_files(rows);
            ui.changed();
        });
    }

    fn recipe(&self) -> ToolRecipe {
        self.choice.borrow().recipe()
    }

    pub(super) fn update(&self) {
        let s = self.state.borrow();
        let recipe = self.recipe();
        let busy = s.requests.busy("operation");
        let ready = self.destination.borrow().is_some() && !self.selected.borrow().is_empty();
        self.execute
            .set_sensitive(s.can_execute() && s.supports(&recipe));
        self.undo
            .set_sensitive(LINUX_TOOL_UNDO_SAFETY_ACCEPTED && s.history.is_some() && !busy);
        self.preview
            .set_sensitive(s.supports(&recipe) && !busy && ready);
        self.preview.set_tooltip_text(Some(if !s.supports(&recipe) {
            "This kind of export isn't available on this computer"
        } else if self.selected.borrow().is_empty() {
            "Tick at least one file first"
        } else if self.destination.borrow().is_none() {
            "Choose a folder to save to first"
        } else {
            "Show the files this export will create, before writing anything"
        }));
        self.execute.set_tooltip_text(Some(if s.preview.is_none() {
            "Press Preview first to check what will be created"
        } else if s.preview_revision != s.revision {
            "Something changed. Press Preview again first."
        } else {
            "Create the new files in the chosen folder (Return)"
        }));
        let exporting = busy && self.running.borrow().is_some();
        self.undo.set_visible(!exporting);
        self.cancel.set_visible(exporting && (recipe.kind == "video" || self.choice.borrow().media_tool().is_some()));
        let sources = self.selected_paths();
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        let disabled = self.choice.borrow().disabled_containers(&sources);
        for index in 0..CONTAINERS.len() {
            self.media_format.set_enabled(index, !disabled.contains(&index));
        }
        self.busy.set_visible(busy);
        let detail = s
            .capabilities
            .iter()
            .find(|c| c.id == recipe.kind)
            .map(|c| {
                if c.available {
                    plain_kind_detail(&recipe.kind).to_owned()
                } else {
                    format!("Unavailable here. {}", c.detail)
                }
            })
            .unwrap_or_else(|| "Checking what this computer can export…".into());
        self.capability.set_text(&detail);
        self.capability.set_tooltip_text(
            s.capabilities
                .iter()
                .find(|c| c.id == recipe.kind)
                .map(|c| c.detail.as_str()),
        );
        self.summary
            .set_text(&self.choice.borrow().summary(self.selected.borrow().len()));
        self.files_count.set_text(&format!(
            "{} of {} chosen",
            self.selected.borrow().len(),
            self.rows.borrow().len()
        ));
    }

    fn changed(&self) {
        self.state.borrow_mut().invalidate();
        self.update();
    }

    pub(super) fn send(&self, action: &str, operation: Option<String>) {
        if action == "undo" && !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
            self.status.set_label(TOOL_UNDO_UNAVAILABLE_REASON);
            return;
        }
        if !matches!(action, "capabilities" | "cancel")
            && self.state.borrow().requests.busy("operation")
        {
            return;
        }
        if action == "preview" && !self.state.borrow().supports(&self.recipe()) {
            self.status
                .set_label("This recipe or format isn't available on this computer.");
            return;
        }
        let mut r = ToolRequest {
            request_id: request_id(),
            action: action.into(),
            file_ids: None,
            destination: None,
            recipe: None,
            operation_id: operation,
            destination_bookmark: None,
        };
        if action == "preview" {
            let ids = self.selected.borrow().clone();
            let Some(destination) = self.destination.borrow().clone() else {
                self.status.set_label("Choose a folder to save to first.");
                return;
            };
            if ids.is_empty() || ids.len() > MAX_FILES {
                self.status.set_label("Choose between 1 and 100 files.");
                return;
            }
            r.file_ids = Some(ids);
            r.destination = Some(destination);
            r.recipe = Some(self.recipe());
            let mut s = self.state.borrow_mut();
            s.preview = None;
            s.preview_revision = s.revision;
        }
        if action == "execute" {
            self.running.replace(r.operation_id.clone());
        }
        {
            let mut state = self.state.borrow_mut();
            let slot = if matches!(action, "capabilities" | "history" | "cancel") {
                action
            } else {
                state.requests.forget("history");
                state.operation_action = Some(action.to_owned());
                "operation"
            };
            state.requests.begin(slot, &r.request_id);
        }
        let sent = self
            .engine
            .borrow_mut()
            .send(CommandPayload::ToolRequest(ToolRequestPayload {
                request: r,
            }));
        if sent.is_err() {
            self.state.borrow_mut().requests.clear();
            self.running.replace(None);
            self.status
                .set_label("FileID is still getting ready. Try again in a moment.");
        } else if !matches!(action, "capabilities" | "history") {
            self.status.set_label("");
        }
        self.update();
    }

    fn set_kind(self: &Rc<Self>, kind: usize) {
        self.choice.borrow_mut().kind = kind;
        self.sync_rows();
        self.selected.borrow_mut().clear();
        self.sync_player();
        self.find_files();
        self.animate_frame();
        self.changed();
    }

    fn selected_paths(&self) -> Vec<String> {
        let rows = self.rows.borrow();
        self.selected
            .borrow()
            .iter()
            .filter_map(|id| rows.iter().find(|(file, _)| file.id == *id).map(|(file, _)| file.path.clone()))
            .collect()
    }

    /// Shows the rows for the chosen kind and video tool.
    fn sync_rows(&self) {
        let choice = self.choice.borrow().clone();
        let kind = KINDS[choice.kind.min(2)];
        let media = choice.media_tool();
        self.tool_row.set_visible(kind == "video");
        self.photo_rows.set_visible(kind == "photo");
        self.video_rows.set_visible(kind == "video" && media.is_none());
        self.media_rows.set_visible(media.is_some());
        self.trim_rows.set_visible(media == Some("trim"));
        self.proxy_row.set_visible(media == Some("proxy"));
        self.rotate_row.set_visible(media == Some("rotate"));
        self.media_note.set_text(media_note(&choice));
        self.format_stack.set_visible_child_name(match media {
            Some("audio") => "audio",
            Some("proxy") => "video",
            Some(_) => "container",
            None => kind,
        });
        self.media_format.select(choice.container.min(1), false);
        self.trim_cut.select(usize::from(choice.trim_exact), false);
    }

    /// Rebuilds the tool picker from the engine's capabilities; unavailable tools are hidden.
    fn rebuild_tools(self: &Rc<Self>) {
        let tools = available_video_tools(&self.state.borrow().capabilities);
        let current = self.choice.borrow().tool;
        let selected = tools.iter().position(|(id, _)| *id == current).unwrap_or(0);
        if tools[selected].0 != current {
            self.choice.borrow_mut().tool = "export";
        }
        v3::clear(&self.tool_slot);
        let labels: Vec<&str> = tools.iter().map(|(_, label)| *label).collect();
        let picker = v3::Segmented::new(&labels, selected);
        picker.root.set_halign(gtk::Align::Start);
        let ids: Vec<&'static str> = tools.iter().map(|(id, _)| *id).collect();
        let weak = Rc::downgrade(self);
        picker.connect_changed(move |index| {
            if let Some(ui) = weak.upgrade() {
                ui.set_tool(ids[index]);
            }
        });
        self.tool_slot.append(&picker.root);
        self.tool_picker.replace(Some(picker));
        self.sync_rows();
        self.sync_player();
        self.show_thumbnail();
        self.animate_frame();
    }

    fn set_tool(self: &Rc<Self>, tool: &'static str) {
        let sources = self.selected_paths();
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        self.choice.borrow_mut().adopt_tool(tool, &sources);
        self.sync_rows();
        self.sync_player();
        self.show_thumbnail();
        self.animate_frame();
        self.changed();
    }

    /// Trim plays the first ticked video so its start and end can come from the playhead.
    fn sync_player(self: &Rc<Self>) {
        let first = self.selected.borrow().first().copied();
        let path = (self.choice.borrow().media_tool() == Some("trim"))
            .then(|| {
                self.rows
                    .borrow()
                    .iter()
                    .find(|(file, _)| Some(file.id) == first && file.kind == "video")
                    .map(|(file, _)| file.path.clone())
            })
            .flatten();
        if *self.player_path.borrow() == path {
            return;
        }
        if let Some(stream) = self.video.media_stream() {
            stream.pause();
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        self.player_path.replace(path.clone());
        let Some(path) = path else {
            self.preview_stack.set_visible_child_name("picture");
            return;
        };
        let media = gtk::MediaFile::for_filename(&path);
        let weak = Rc::downgrade(self);
        media.connect_prepared_notify(move |stream| {
            let Some(ui) = weak.upgrade() else { return };
            if !stream.is_prepared() {
                return;
            }
            let duration = stream.duration() as f64 / 1_000_000.0;
            let (start, end) = {
                let choice = ui.choice.borrow();
                (choice.trim_start, choice.trim_end)
            };
            if duration > 0.0 && end <= start {
                ui.trim_end.set_value((duration * 100.0).floor() / 100.0);
            }
        });
        self.video.set_media_stream(Some(&media));
        self.preview_stack.set_visible_child_name("video");
    }

    fn playhead(&self) -> Option<f64> {
        self.video
            .media_stream()
            .filter(|stream| stream.is_prepared())
            .map(|stream| stream.timestamp() as f64 / 1_000_000.0)
    }

    /// The last thumbnail, turned as the rotate tool would show it.
    fn show_thumbnail(&self) {
        let Some(decoded) = self.thumbnail.borrow().clone() else {
            self.picture.set_paintable(None::<&gtk::gdk::Texture>);
            return;
        };
        let turns = self.choice.borrow().preview_turns();
        let decoded = if turns == 0 {
            decoded
        } else {
            let (rgba, width, height) =
                turn_rgba(&decoded.rgba, decoded.width as usize, decoded.height as usize, turns);
            DecodedImage { width: width as u32, height: height as u32, rgba }
        };
        self.picture.set_paintable(Some(&texture_from_decoded(decoded)));
    }

    fn animate_frame(&self) {
        let to = self.choice.borrow().frame_ratio(self.photo_aspect.get());
        let from = self.frame_ratio.get();
        if (to - from).abs() < 0.001 {
            return;
        }
        if let Some(previous) = self.frame_animation.take() {
            previous.pause();
        }
        let frame = self.frame.clone();
        let ratio = self.frame_ratio.clone();
        let animation = animate_with(Spring::Standard, &self.frame, from, to, move |value| {
            let value = value.max(0.2) as f32;
            ratio.set(f64::from(value));
            frame.set_ratio(value);
        });
        self.frame_animation.replace(Some(animation));
    }

    fn find_files(self: &Rc<Self>) {
        let lookup = self.begin_lookup();
        let kind = self.choice.borrow().file_kind();
        let rx = self.engine.borrow().query_files(QuerySpec {
            search: self.query.text().trim().to_owned(),
            kind: kind.map(str::to_owned),
            limit: 500,
            offset: 0,
            channel: QueryChannel::Tools,
            scope: None,
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = rx.recv().await;
            let Some(ui) = weak.upgrade() else { return };
            if !ui.end_lookup(lookup) {
                return;
            }
            match result {
                Ok(Ok((files, _))) => ui.render_files(files),
                Ok(Err(error)) => ui
                    .status
                    .set_label(&format!("Library unavailable: {error}")),
                Err(_) => {}
            }
        });
    }

    fn render_files(self: &Rc<Self>, files: Vec<FileRow>) {
        v3::clear(&self.files);
        let mut rows = Vec::new();
        if files.is_empty() {
            let empty = v3::meta("No matching files. Search by name, or scan a folder first.");
            empty.add_css_class("v3-row-pad");
            self.files.append(&empty);
        }
        let selected = self.selected.borrow().clone();
        for file in files {
            let check = gtk::CheckButton::builder()
                .active(selected.contains(&file.id))
                .valign(gtk::Align::Center)
                .build();
            check.update_property(&[gtk::accessible::Property::Label(&file.name)]);
            let row = v3::list_row(10);
            row.append(&check);
            let text = v3::vbox(1);
            text.append(&v3::mono(&file.name, "s12"));
            let parent = v3::mono(&v3::parent_name(&file.path), "s11");
            parent.add_css_class("tone-muted");
            parent.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            text.append(&parent);
            row.append(&text);
            let click = gtk::GestureClick::new();
            let toggle = check.downgrade();
            click.connect_released(move |_, _, _, _| {
                if let Some(check) = toggle.upgrade() {
                    check.set_active(!check.is_active());
                }
            });
            text.add_controller(click);
            let weak = Rc::downgrade(self);
            let id = file.id;
            check.connect_toggled(move |check| {
                let Some(ui) = weak.upgrade() else { return };
                {
                    let mut ids = ui.selected.borrow_mut();
                    ids.retain(|other| *other != id);
                    if check.is_active() && ids.len() < MAX_FILES {
                        ids.push(id);
                    } else if check.is_active() {
                        check.set_active(false);
                        ui.status.set_label("Up to 100 files per export.");
                    }
                }
                ui.load_preview_picture();
                ui.changed();
            });
            self.files.append(&row);
            rows.push((file, check));
        }
        self.rows.replace(rows);
        self.load_preview_picture();
        self.update();
    }

    fn select_all(&self, on: bool) {
        let checks: Vec<gtk::CheckButton> = self
            .rows
            .borrow()
            .iter()
            .map(|(_, check)| check.clone())
            .collect();
        for check in checks.iter().take(if on { MAX_FILES } else { usize::MAX }) {
            check.set_active(on);
        }
    }

    fn load_preview_picture(self: &Rc<Self>) {
        let first = self.selected.borrow().first().copied();
        let row = first.and_then(|id| {
            self.rows
                .borrow()
                .iter()
                .find(|(file, _)| file.id == id)
                .map(|(file, _)| file.clone())
        });
        self.sync_player();
        let Some(row) = row else {
            self.thumbnail.replace(None);
            self.picture.set_paintable(None::<&gtk::gdk::Texture>);
            return;
        };
        let rx = self
            .engine
            .borrow()
            .request_preview(row.path.clone(), super::util::decode_px(&self.picture, 900), row.kind == "video");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(ThumbReply::Image(decoded)) = rx.recv().await else {
                return;
            };
            let Some(ui) = weak.upgrade() else { return };
            if decoded.height > 0 {
                ui.photo_aspect
                    .set(Some(f64::from(decoded.width) / f64::from(decoded.height)));
            }
            ui.thumbnail.replace(Some(decoded));
            ui.show_thumbnail();
            ui.animate_frame();
        });
    }

    fn render_outputs(&self, title: &str, outputs: &[ToolOutput]) {
        v3::clear(&self.outputs);
        let header = v3::section(&format!("Outputs · {}", outputs.len()));
        header.add_css_class("v3-row-pad");
        header.set_tooltip_text(Some(title));
        self.outputs.append(&header);
        for output in outputs {
            let row = v3::vbox(2);
            row.add_css_class("v3-row-pad");
            row.add_css_class("v3-soft-row");
            let head = v3::hbox(8);
            let name = v3::mono(&v3::file_name(&output.output_path), "s12");
            name.set_hexpand(true);
            name.set_tooltip_text(Some(&output.output_path));
            head.append(&name);
            let (state, tone) = output_state(&output.state);
            head.append(&v3::label(state, &["v3-state-115", tone.css()]));
            row.append(&head);
            if !output.message.is_empty() {
                row.append(&v3::wrapped(&output.message, &["v3-meta-115"]));
            }
            self.outputs.append(&row);
        }
    }
}

#[cfg(test)]
impl ToolsUi {
    pub(super) fn use_destination(&self, path: &str) {
        self.destination.replace(Some(path.to_owned()));
        self.destination_label.set_text(path);
        self.changed();
    }
}

pub(super) fn output_state(state: &str) -> (&'static str, Tone) {
    match state {
        "pending" | "planned" => ("Planned", Tone::Muted),
        "running" => ("Running", Tone::Gold),
        "completed" => ("Completed", Tone::Done),
        "failed" => ("Failed", Tone::Danger),
        "undone" => ("Undone", Tone::Paused),
        _ => ("Planned", Tone::Muted),
    }
}

fn output_row(label: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = v3::hbox(12);
    let caption = v3::label(label, &["v3-output-label"]);
    caption.set_size_request(92, -1);
    caption.set_valign(gtk::Align::Center);
    row.append(&caption);
    row.append(control);
    row
}

pub fn tools(engine: Engine) -> gtk::Widget {
    build_tools(engine).0
}

pub(super) fn build_tools(engine: Engine) -> (gtk::Widget, Rc<ToolsUi>) {
    let left = v3::vbox(16);
    left.add_css_class("v3-page-pad");

    // ── 01 · Pick files ────────────────────────────────────────────────────────
    let recipe_panel = v3::panel("01 · Pick files");
    let kind = v3::Segmented::new(&["Photo", "Video", "Chapters"], 0);
    recipe_panel.trailing.append(&kind.root);
    let search_row = v3::hbox(8);
    search_row.add_css_class("v3-panel-pad");
    let query = v3::text_field("Search by name or what's in it");
    query.set_primary_icon_name(Some("system-search-symbolic"));
    search_row.append(&query);
    let find = v3::secondary("Find", true);
    search_row.append(&find);
    recipe_panel.body.append(&search_row);
    let files = v3::vbox(0);
    let files_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(180)
        .max_content_height(320)
        .propagate_natural_height(true)
        .child(&files)
        .build();
    recipe_panel.body.append(&files_scroll);
    let recipe_footer = recipe_panel.show_footer();
    let files_count = v3::meta("0 of 0 chosen");
    files_count.add_css_class("numeric");
    files_count.set_hexpand(true);
    recipe_footer.append(&files_count);
    let select_all = v3::ghost("Select all");
    let select_none = v3::ghost("Select none");
    recipe_footer.append(&select_all);
    recipe_footer.append(&select_none);
    left.append(&recipe_panel.root);

    // ── 02 · Output ────────────────────────────────────────────────────────
    let output_panel = v3::panel("02 · Output");
    let output_body = v3::vbox(10);
    output_body.add_css_class("v3-panel-pad");
    let photo_format = v3::Segmented::new(&PHOTO_FORMATS.map(|(label, _)| label), 0);
    let chapter_format = v3::Segmented::new(&CHAPTER_FORMATS.map(|(label, _)| label), 0);
    let format_stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .halign(gtk::Align::Start)
        .build();
    format_stack.add_named(&photo_format.root, Some("photo"));
    format_stack.add_named(&v3::label("MP4 · H.264", &["v3-body"]), Some("video"));
    format_stack.add_named(&chapter_format.root, Some("chapters"));
    let media_format = v3::Segmented::new(&CONTAINERS.map(|(label, _)| label), 0);
    format_stack.add_named(&media_format.root, Some("container"));
    format_stack.add_named(&v3::label("M4A · AAC", &["v3-body"]), Some("audio"));
    let tool_slot = v3::hbox(0);
    let tool_row = output_row("Tool", &tool_slot);
    tool_row.set_visible(false);
    output_body.append(&tool_row);
    output_body.append(&output_row("Format", &format_stack));
    let photo_rows = v3::vbox(10);
    let longest_side = v3::number_field(1.0, 8192.0, 1.0, 0);
    longest_side.set_value(2048.0);
    let size_row = v3::hbox(8);
    size_row.append(&longest_side);
    size_row.append(&v3::meta("px, up to 8192"));
    photo_rows.append(&output_row("Longest side", &size_row));
    let (enlarge_row, enlarge) = v3::toggle_row("Enlarge smaller media to this size");
    photo_rows.append(&enlarge_row);
    output_body.append(&photo_rows);
    let video_rows = v3::vbox(10);
    video_rows.set_visible(false);
    let resolution = v3::Segmented::new(&RESOLUTIONS.map(|(label, _)| label), 1);
    resolution.root.set_halign(gtk::Align::Start);
    video_rows.append(&output_row("Resolution", &resolution.root));
    let frames_box = v3::hbox(8);
    let mut frames = Vec::new();
    for (index, (name, _, ratio)) in FRAMES.iter().enumerate() {
        let tile = v3::vbox(6);
        let (w, h) = if *ratio >= 1.0 {
            (30.0 * ratio.min(1.9), 30.0)
        } else {
            (30.0 * ratio, 30.0)
        };
        let shape = gtk::Box::builder()
            .width_request(w as i32)
            .height_request(h as i32)
            .halign(gtk::Align::Center)
            .css_classes(["v3-aspect-frame"])
            .build();
        tile.append(&shape);
        tile.append(&v3::label(name, &[]));
        let button = gtk::Button::builder()
            .child(&tile)
            .css_classes(["v3-aspect"])
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&format!("Frame {name}"))]);
        v3::set_selected(&button, index == 0);
        frames_box.append(&button);
        frames.push(button);
    }
    video_rows.append(&output_row("Frame aspect", &frames_box));
    let notes = v3::wrapped(
        "Frames other than Original fit the whole picture with padding. Stabilizing shaky video isn't available on Linux yet.",
        &["v3-meta"],
    );
    notes.set_margin_start(104);
    video_rows.append(&notes);
    let (stabilize_row, stabilize) = v3::toggle_row("Stabilize shaky video");
    stabilize.set_sensitive(false);
    stabilize_row.set_tooltip_text(Some("Not available in the Linux engine yet"));
    video_rows.append(&stabilize_row);
    output_body.append(&video_rows);
    let media_rows = v3::vbox(10);
    media_rows.set_visible(false);
    let trim_rows = v3::vbox(10);
    let trim_start = v3::number_field(0.0, 21_600.0, 0.1, 2);
    let trim_end = v3::number_field(0.0, 21_600.0, 0.1, 2);
    let start_here = v3::ghost("Set from playhead");
    let end_here = v3::ghost("Set from playhead");
    for (label, field, button) in [("Start", &trim_start, &start_here), ("End", &trim_end, &end_here)] {
        field.update_property(&[gtk::accessible::Property::Label(&format!("{label} time in seconds"))]);
        button.set_tooltip_text(Some("Use the player's current time"));
        let row = v3::hbox(8);
        row.append(field);
        row.append(&v3::meta("s"));
        row.append(button);
        trim_rows.append(&output_row(label, &row));
    }
    let trim_cut = v3::Segmented::new(&["Keyframe", "Exact"], 0);
    trim_cut.root.set_halign(gtk::Align::Start);
    trim_rows.append(&output_row("Cut", &trim_cut.root));
    media_rows.append(&trim_rows);
    let proxy_size = v3::Segmented::new(&PROXY_SIDES.map(|(label, _)| label), 0);
    proxy_size.root.set_halign(gtk::Align::Start);
    let proxy_row = output_row("Size", &proxy_size.root);
    media_rows.append(&proxy_row);
    let rotation = v3::Segmented::new(&ROTATIONS.map(|(label, _)| label), 0);
    rotation.root.set_halign(gtk::Align::Start);
    let rotate_row = output_row("Turn", &rotation.root);
    media_rows.append(&rotate_row);
    let media_note = v3::wrapped("", &["v3-meta"]);
    media_note.set_margin_start(104);
    media_rows.append(&media_note);
    output_body.append(&media_rows);
    output_panel.body.append(&output_body);
    let output_footer = output_panel.show_footer();
    output_footer.append(&v3::section("Save to"));
    let destination_label = v3::mono("No folder chosen", "s12");
    destination_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    destination_label.set_hexpand(true);
    output_footer.append(&destination_label);
    let choose = v3::secondary("Choose…", false);
    choose.set_tooltip_text(Some("Choose a folder to save to"));
    choose.update_property(&[gtk::accessible::Property::Label("Choose output folder")]);
    output_footer.append(&choose);
    left.append(&output_panel.root);
    let status = v3::wrapped("", &["v3-meta"]);
    left.append(&status);
    let left_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&left)
        .build();

    // ── 03 · Preview ───────────────────────────────────────────────────────
    let pane = v3::vbox(0);
    pane.add_css_class("v3-pane");
    pane.set_size_request(380, -1);
    let preview_head = v3::hbox(8);
    preview_head.add_css_class("v3-sources-header");
    let preview_title = v3::section("03 · Preview");
    preview_title.set_hexpand(true);
    preview_head.append(&preview_title);
    let busy = v3::busy_bar(60);
    preview_head.append(&busy);
    pane.append(&preview_head);
    let preview_body = v3::vbox(10);
    preview_body.add_css_class("v3-panel-pad");
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .build();
    let video = gtk::Video::builder()
        .autoplay(false)
        .css_classes(["v3-black"])
        .build();
    let preview_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    preview_stack.add_named(&picture, Some("picture"));
    preview_stack.add_named(&video, Some("video"));
    let frame = gtk::AspectFrame::builder()
        .ratio(4.0 / 3.0)
        .obey_child(false)
        .child(&preview_stack)
        .css_classes(["v3-preview-frame"])
        .build();
    let frame_clamp = gtk::Box::builder().height_request(220).build();
    frame.set_hexpand(true);
    frame_clamp.append(&frame);
    preview_body.append(&frame_clamp);
    let summary = v3::label("", &["v3-title-13"]);
    preview_body.append(&summary);
    let capability = v3::wrapped("", &["v3-meta"]);
    preview_body.append(&capability);
    let outputs = v3::vbox(0);
    preview_body.append(&outputs);
    let pane_scroll = v3::scroller(&preview_body);
    pane.append(&pane_scroll);
    let actions = v3::vbox(8);
    actions.add_css_class("v3-panel-pad");
    let action_row = v3::hbox(8);
    let preview = v3::secondary("Preview", true);
    let execute = v3::primary("Export", false);
    execute.set_tooltip_text(Some("Export (Return)"));
    let undo = v3::accent("Undo", Tone::Gold, true);
    if !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        undo.set_tooltip_text(Some(TOOL_UNDO_UNAVAILABLE_REASON));
    }
    let cancel = v3::ghost("Cancel");
    cancel.set_visible(false);
    action_row.append(&preview);
    action_row.append(&execute);
    action_row.append(&v3::spacer());
    action_row.append(&undo);
    action_row.append(&cancel);
    actions.append(&action_row);
    let history_row = v3::hbox(8);
    let last = v3::ghost("Show last export");
    history_row.append(&last);
    history_row.append(&v3::wrapped(
        "Exports are new files; your originals stay as they are.",
        &["v3-meta"],
    ));
    actions.append(&history_row);
    pane.append(&actions);

    let root = v3::hbox(0);
    root.append(&left_scroll);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    root.append(&pane);

    let ui = Rc::new(ToolsUi {
        engine: engine.clone(),
        state: RefCell::new(ExportState::default()),
        status,
        outputs,
        execute,
        undo,
        preview,
        choice: RefCell::new(RecipeChoice {
            kind: 0,
            format: 0,
            longest_side: 2048,
            resolution: 1,
            frame: 0,
            enlarge: false,
            tool: "export",
            trim_start: 0.0,
            trim_end: 0.0,
            trim_exact: false,
            container: 0,
            proxy: 0,
            rotation: 0,
        }),
        kind,
        photo_format,
        chapter_format,
        resolution,
        longest_side,
        enlarge,
        frames,
        photo_rows,
        video_rows,
        format_stack,
        query,
        files,
        files_count,
        rows: RefCell::new(Vec::new()),
        selected: RefCell::new(Vec::new()),
        destination: RefCell::new(None),
        destination_label,
        frame,
        frame_ratio: Rc::new(Cell::new(4.0 / 3.0)),
        frame_animation: RefCell::new(None),
        picture,
        photo_aspect: Cell::new(None),
        summary,
        capability,
        busy,
        cancel,
        running: RefCell::new(None),
        lookup: Cell::new(0),
        loading: Cell::new(false),
        handoff: RefCell::new(None),
        tool_row,
        tool_slot,
        tool_picker: RefCell::new(None),
        media_format,
        media_rows,
        trim_rows,
        trim_start,
        trim_end,
        trim_cut,
        proxy_row,
        proxy_size,
        rotate_row,
        rotation,
        media_note,
        preview_stack,
        video,
        thumbnail: RefCell::new(None),
        player_path: RefCell::new(None),
    });
    let weak = Rc::downgrade(&ui);
    let receiver: Rc<dyn Fn(Vec<i64>)> = Rc::new(move |ids| {
        if let Some(ui) = weak.upgrade() {
            ui.receive(ids);
        }
    });
    super::chat_handoff::receive_in_tools(&receiver);
    ui.handoff.replace(Some(receiver));
    wire(&ui, &find, &select_all, &select_none, &choose, &last, &root);
    wire_media(&ui, &start_here, &end_here);
    ui.rebuild_tools();
    ui.update();
    (root.upcast(), ui)
}

fn wire(
    ui: &Rc<ToolsUi>,
    find: &gtk::Button,
    select_all: &gtk::Button,
    select_none: &gtk::Button,
    choose: &gtk::Button,
    last: &gtk::Button,
    root: &gtk::Box,
) {
    let weak = Rc::downgrade(ui);
    ui.kind.connect_changed(move |kind| {
        if let Some(ui) = weak.upgrade() {
            ui.set_kind(kind);
        }
    });
    for (segmented, field) in [
        (&ui.photo_format, 0),
        (&ui.chapter_format, 1),
        (&ui.resolution, 2),
    ] {
        let weak = Rc::downgrade(ui);
        segmented.connect_changed(move |index| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut choice = ui.choice.borrow_mut();
                match field {
                    0 | 1 => choice.format = index,
                    _ => choice.resolution = index,
                }
            }
            ui.changed();
        });
    }
    let weak = Rc::downgrade(ui);
    ui.longest_side.connect_value_changed(move |field| {
        if let Some(ui) = weak.upgrade() {
            ui.choice.borrow_mut().longest_side = field.value() as u32;
            ui.changed();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.enlarge.connect_active_notify(move |switch| {
        if let Some(ui) = weak.upgrade() {
            ui.choice.borrow_mut().enlarge = switch.is_active();
            ui.changed();
        }
    });
    for (index, button) in ui.frames.iter().enumerate() {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            ui.choice.borrow_mut().frame = index;
            for (other, frame) in ui.frames.iter().enumerate() {
                v3::set_selected(frame, other == index);
            }
            ui.animate_frame();
            ui.changed();
        });
    }
    let weak = Rc::downgrade(ui);
    find.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.find_files();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.query.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.find_files();
        }
    });
    for (button, on) in [(select_all, true), (select_none, false)] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.select_all(on);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    choose.connect_clicked(move |button| {
        let dialog = gtk::FileDialog::builder()
            .title("Choose a folder to save to")
            .build();
        let window = button.root().and_downcast::<gtk::Window>();
        let weak = weak.clone();
        dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let (Some(ui), Some(path)) = (weak.upgrade(), result.ok().and_then(|file| file.path()))
            else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            ui.destination_label.set_text(&path);
            ui.destination_label.set_tooltip_text(Some(&path));
            ui.destination.replace(Some(path));
            ui.changed();
        });
    });
    let weak = Rc::downgrade(ui);
    ui.preview.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.send("preview", None);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.execute.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if !ui.state.borrow().can_execute() || !ui.state.borrow().supports(&ui.recipe()) {
            return;
        }
        let id = ui.state.borrow_mut().preview.take();
        ui.send("execute", id);
    });
    let weak = Rc::downgrade(ui);
    ui.undo.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let id = ui.state.borrow().history.clone();
            ui.send("undo", id);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.cancel.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let id = ui.running.borrow().clone();
            ui.send("cancel", id);
        }
    });
    let weak = Rc::downgrade(ui);
    last.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.send("history", None);
        }
    });
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
            && ui.execute.is_sensitive()
        {
            ui.execute.emit_clicked();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    root.add_controller(keys);
    let weak = Rc::downgrade(ui);
    root.connect_map(move |_| {
        if let Some(ui) = weak.upgrade().filter(|ui| ui.rows.borrow().is_empty() && !ui.loading.get()) {
            ui.find_files();
        }
    });

    let rx = ui.engine.borrow_mut().subscribe();
    let u = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                EngineState::Ready => {
                    u.send("capabilities", None);
                    u.send("history", None);
                }
                EngineState::ToolResponse(r) => handle_response(&u, r),
                EngineState::Failed(e) => {
                    u.state.borrow_mut().requests.clear();
                    u.state.borrow_mut().preview = None;
                    u.running.replace(None);
                    u.status.set_label(&e);
                    u.update();
                }
                EngineState::Exited => {
                    u.state.borrow_mut().requests.clear();
                    u.state.borrow_mut().preview = None;
                    u.running.replace(None);
                    u.status.set_label(
                        "FileID's background helper stopped. Show the last export before trying an interrupted one again.",
                    );
                    u.update();
                }
                _ => {}
            }
        }
    });
}

fn wire_media(ui: &Rc<ToolsUi>, start_here: &gtk::Button, end_here: &gtk::Button) {
    for (field, start) in [(&ui.trim_start, true), (&ui.trim_end, false)] {
        let weak = Rc::downgrade(ui);
        field.connect_value_changed(move |field| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut choice = ui.choice.borrow_mut();
                if start {
                    choice.trim_start = field.value();
                } else {
                    choice.trim_end = field.value();
                }
            }
            ui.changed();
        });
    }
    for (button, start) in [(start_here, true), (end_here, false)] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(seconds) = ui.playhead() else {
                ui.status.set_label("Tick a video that can play here first.");
                return;
            };
            let seconds = (seconds * 100.0).round() / 100.0;
            if start {
                ui.trim_start.set_value(seconds);
            } else {
                ui.trim_end.set_value(seconds);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    ui.media_format.connect_changed(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        ui.choice.borrow_mut().container = index;
        ui.changed();
    });
    let weak = Rc::downgrade(ui);
    ui.trim_cut.connect_changed(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        let sources = ui.selected_paths();
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        {
            let mut choice = ui.choice.borrow_mut();
            choice.trim_exact = index == 1;
            choice.settle_container(&sources);
        }
        ui.sync_rows();
        ui.changed();
    });
    let weak = Rc::downgrade(ui);
    ui.proxy_size.connect_changed(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        ui.choice.borrow_mut().proxy = index;
        ui.changed();
    });
    let weak = Rc::downgrade(ui);
    ui.rotation.connect_changed(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        ui.choice.borrow_mut().rotation = index;
        ui.show_thumbnail();
        ui.animate_frame();
        ui.changed();
    });
}

fn media_note(choice: &RecipeChoice) -> &'static str {
    match choice.media_tool() {
        Some("trim") if choice.trim_exact => {
            "Re-encodes to an H.264 MP4 so the cut lands on the exact frame. HDR videos are refused."
        }
        Some("trim") => {
            "Copies the video without re-encoding. The new file may start a little early, at the keyframe before your start time."
        }
        Some("remux") => {
            "Moves the video and sound into the other container without re-encoding. The container a file already has is greyed out."
        }
        Some("audio") => "Saves the soundtrack as AAC in an M4A file. Videos without sound are refused.",
        Some("proxy") => {
            "A small H.264 copy for quick previews, linked to its original. Smaller videos aren't enlarged."
        }
        Some(_) => "Changes which way up the picture is shown, without re-encoding it.",
        None => "",
    }
}

fn handle_response(u: &Rc<ToolsUi>, r: ToolResponse) {
    let action = {
        let mut state = u.state.borrow_mut();
        let Some(slot) = state.requests.accept(&r.request_id, true) else {
            return;
        };
        if slot == "operation" {
            state.operation_action.take().unwrap_or_default()
        } else {
            slot
        }
    };
    let action = action.as_str();
    if action == "execute" {
        u.running.replace(None);
    }
    if action != "capabilities" || r.status != "ok" {
        u.status.set_label(&r.message);
    }
    {
        let mut s = u.state.borrow_mut();
        if action == "capabilities" && r.status == "ok" {
            s.capabilities = r.capabilities.clone();
        }
        if action == "preview" && r.status == "ok" && s.preview_revision == s.revision {
            s.preview = r.operation_id.clone();
        }
        if action == "history" {
            s.invalidate();
        }
        if matches!(action, "execute" | "history") && (r.status == "ok" || !r.outputs.is_empty()) {
            s.history = if r.outputs.iter().any(|o| o.state == "completed") {
                r.operation_id.clone()
            } else {
                None
            };
        }
        if action == "undo" && r.status == "ok" {
            s.history = None;
        }
    }
    if action == "capabilities" && r.status == "ok" {
        u.rebuild_tools();
    }
    let replace_receipts =
        !matches!(action, "capabilities" | "cancel") && (r.status == "ok" || !r.outputs.is_empty());
    if replace_receipts {
        let title = format!(
            "{action} operation {}",
            r.operation_id.as_deref().unwrap_or("none")
        );
        u.render_outputs(&title, &r.outputs);
    }
    u.update();
}

fn plain_kind_detail(kind: &str) -> &'static str {
    match kind {
        "trim" => "Keeps the part between the start and end times as a new file. Your original video stays as it is.",
        "remux" => "Puts the same video and sound in a different container. Your original video stays as it is.",
        "audio" => "Saves the soundtrack as a new audio file. Your original video stays as it is.",
        "proxy" => "Makes a small preview copy linked to the original. Your original video stays as it is.",
        "rotate" => "Saves a copy that plays the other way up. Your original video stays as it is.",
        "video" => "Makes a new MP4 at the size and shape you choose. Your original video stays as it is.",
        "chapters" => "Saves a video's chapters as a file other apps can read.",
        _ => "Makes a new copy at the format and size you choose. Location and camera details are removed, and your original stays as it is.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handed_files_pick_a_fitting_kind_and_report_the_limit() {
        assert_eq!(kind_for(&["image", "image"]), Some(0));
        assert_eq!(kind_for(&["video"]), Some(1));
        assert_eq!(kind_for(&["image", "video"]), None);
        assert_eq!(kind_for(&[]), None);
        assert_eq!(handoff_status(1, 1), "Chat chose 1 file. Pick a format, then Preview.");
        assert_eq!(
            handoff_status(100, 250),
            "Chat chose 250 files; the first 100 are ticked. Up to 100 files per export."
        );
    }

    fn choice(kind: usize) -> RecipeChoice {
        RecipeChoice {
            kind,
            format: 0,
            longest_side: 4096,
            resolution: 1,
            frame: 0,
            enlarge: true,
            tool: "export",
            trim_start: 0.0,
            trim_end: 0.0,
            trim_exact: false,
            container: 0,
            proxy: 0,
            rotation: 0,
        }
    }

    fn capability(id: &str, available: bool) -> ToolCapability {
        ToolCapability {
            id: id.into(),
            available,
            input_formats: vec![],
            output_formats: vec![],
            detail: String::new(),
        }
    }

    #[test]
    fn tool_picker_hides_unavailable_tools() {
        let ids = |caps: &[ToolCapability]| available_video_tools(caps).into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(ids(&[]), ["export"]);
        let all: Vec<_> = ["trim", "remux", "audio", "proxy", "rotate"].map(|id| capability(id, true)).into();
        assert_eq!(ids(&all), ["export", "trim", "remux", "audio", "proxy", "rotate"]);
        assert_eq!(
            ids(&[capability("trim", false), capability("rotate", true), capability("photo", true)]),
            ["export", "rotate"]
        );
    }

    #[test]
    fn video_tools_send_only_the_fields_their_recipe_takes() {
        let mut video = RecipeChoice { frame: 2, ..choice(1) };
        video.trim_start = 1.0;
        video.rotation = 2;
        let export = video.recipe();
        assert_eq!((export.kind.as_str(), export.video_aspect_ratio.as_deref()), ("video", Some("9:16")));
        assert!(export.trim_start_seconds.is_none() && export.trim_mode.is_none() && export.rotation_degrees.is_none());

        video.adopt_tool("trim", &["/v/clip.mov"]);
        video.trim_start = 1.25;
        video.trim_end = 4.5;
        let trim = video.recipe();
        assert_eq!((trim.kind.as_str(), trim.format.as_str(), trim.max_dimension), ("trim", "mov", 1920));
        assert_eq!((trim.trim_start_seconds, trim.trim_end_seconds, trim.trim_mode.as_deref()), (Some(1.25), Some(4.5), Some("keyframe")));
        assert!(trim.video_aspect_ratio.is_none() && trim.allow_upscale.is_none() && trim.rotation_degrees.is_none());
        video.trim_exact = true;
        assert_eq!(video.recipe().format, "mp4");
        assert_eq!(video.disabled_containers(&["/v/clip.mov"]), [1]);

        video.adopt_tool("audio", &[]);
        let audio = video.recipe();
        assert_eq!((audio.kind.as_str(), audio.format.as_str()), ("audio", "m4a"));
        assert!(audio.trim_start_seconds.is_none() && audio.trim_mode.is_none());

        video.adopt_tool("proxy", &[]);
        video.proxy = 1;
        let proxy = video.recipe();
        assert_eq!((proxy.kind.as_str(), proxy.format.as_str(), proxy.max_dimension), ("proxy", "mp4", 960));

        video.adopt_tool("rotate", &["/v/clip.mp4"]);
        for (index, degrees, turns) in [(0, 90, 1), (1, 180, 2), (2, 270, 3)] {
            video.rotation = index;
            let rotate = video.recipe();
            assert_eq!((rotate.kind.as_str(), rotate.format.as_str(), rotate.rotation_degrees), ("rotate", "mp4", Some(degrees)));
            assert_eq!(video.preview_turns(), turns);
        }
        assert_eq!(video.summary(1), "1 file · Rotate · MP4 · 90° left");
        video.rotation = 0;
        assert_eq!(video.frame_ratio(Some(16.0 / 9.0)), 9.0 / 16.0);
        assert_eq!(RecipeChoice { kind: 0, ..video.clone() }.recipe().kind, "photo");
    }

    #[test]
    fn remux_greys_out_the_container_the_sources_already_have() {
        let mut video = RecipeChoice { container: 1, ..choice(1) };
        video.adopt_tool("remux", &["/v/clip.MOV"]);
        assert_eq!(video.disabled_containers(&["/v/clip.MOV"]), [1]);
        assert_eq!(video.recipe().format, "mp4");
        video.settle_container(&["/v/phone.m4v"]);
        assert_eq!(video.recipe().format, "mov");
        assert_eq!(video.disabled_containers(&["/v/a.mp4", "/v/b.mov"]), [0, 1]);
        assert_eq!(source_container("/v/notes.txt"), None);
    }

    #[test]
    fn rotated_previews_turn_clockwise() {
        // 2x1 image: red then green. A quarter turn clockwise puts red on top.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let (turned, width, height) = turn_rgba(&rgba, 2, 1, 1);
        assert_eq!((width, height), (1, 2));
        assert_eq!(turned, [255, 0, 0, 255, 0, 255, 0, 255]);
        let (turned, _, _) = turn_rgba(&rgba, 2, 1, 3);
        assert_eq!(turned, [0, 255, 0, 255, 255, 0, 0, 255]);
        let (turned, width, height) = turn_rgba(&rgba, 2, 1, 2);
        assert_eq!((width, height, turned), (2, 1, vec![0, 255, 0, 255, 255, 0, 0, 255]));
    }

    #[test]
    fn recipes_only_carry_what_the_engine_accepts() {
        let photo = choice(0).recipe();
        assert_eq!(
            (
                photo.kind.as_str(),
                photo.format.as_str(),
                photo.max_dimension
            ),
            ("photo", "png", 4096)
        );
        assert_eq!(photo.allow_upscale, Some(true));
        assert_eq!(photo.video_aspect_ratio, None);
        let video = RecipeChoice {
            frame: 2,
            resolution: 0,
            ..choice(1)
        }
        .recipe();
        assert_eq!((video.format.as_str(), video.max_dimension), ("mp4", 1280));
        assert_eq!(video.video_aspect_ratio.as_deref(), Some("9:16"));
        assert_eq!(video.allow_upscale, None);
        assert!(video.video_framing.is_none() && video.video_stabilization.is_none());
        assert_eq!(choice(1).recipe().video_aspect_ratio, None);
        let chapters = choice(2).recipe();
        assert_eq!(
            (chapters.kind.as_str(), chapters.format.as_str()),
            ("chapters", "vtt")
        );
    }

    #[test]
    fn summary_and_frame_follow_the_recipe() {
        assert_eq!(choice(0).summary(3), "3 files · PNG · ≤ 4096 px");
        assert_eq!(
            RecipeChoice {
                frame: 3,
                ..choice(1)
            }
            .summary(1),
            "1 file · MP4 · H.264 · 1080p · 1:1"
        );
        assert_eq!(choice(2).summary(2), "2 files · WebVTT");
        assert_eq!(choice(0).frame_ratio(Some(1.5)), 1.5);
        assert_eq!(choice(0).frame_ratio(None), 4.0 / 3.0);
        assert_eq!(
            RecipeChoice {
                frame: 2,
                ..choice(1)
            }
            .frame_ratio(None),
            9.0 / 16.0
        );
        assert_eq!(output_state("completed"), ("Completed", Tone::Done));
    }
}
