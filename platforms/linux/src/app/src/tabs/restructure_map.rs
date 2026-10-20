// Restructure folder map (macOS `SankeyFlowView` + `TreeDiffView`): the
// "Folder map" Sankey with focusable From / To nodes and hoverable ribbons,
// and the "Now → After" tree with its Show filter, both wired to the shared
// hover bus so the map, tree, recommendations and Staying put light together.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::restructure_map_layout::*;
use super::restructure_model::Outcome;
use crate::shell::status::Tone;
use crate::v3;

const NODE_WIDTH: f64 = 196.0;
const TREE_ROW_CAP: usize = 400;
const NEUTRAL: (f64, f64, f64) = (0.651, 0.675, 0.702);

#[derive(Debug, Clone, PartialEq)]
pub enum Drill {
    Source(String),
    Bucket(String),
    Sources(Vec<String>),
    Buckets(Vec<String>),
}

pub struct MapView {
    pub root: gtk::Box,
    panel: v3::Panel,
    meta: gtk::Label,
    hint: gtk::Label,
    stack: gtk::Stack,
    ribbons: gtk::DrawingArea,
    nodes: gtk::Fixed,
    overlay: gtk::Overlay,
    from_count: gtk::Label,
    to_count: gtk::Label,
    layout: Rc<RefCell<MapLayout>>,
    buttons: RefCell<Vec<(Node, gtk::Button)>>,
    height: Rc<Cell<f64>>,
    entrance: RefCell<Option<adw::SpringAnimation>>,
    filter: Rc<v3::Segmented>,
    today: gtk::Box,
    proposed: gtk::Box,
    items: RefCell<Vec<MapItem>>,
    staying: RefCell<Vec<(String, String, u64)>>,
    bus: Rc<HoverBus>,
    on_drill: Rc<dyn Fn(Drill)>,
}

fn tint_rgb(tint: Option<usize>) -> (f64, f64, f64) {
    tint.map_or(NEUTRAL, |index| PALETTE[index % PALETTE.len()])
}

fn swatch(rgb: (f64, f64, f64), icon: &str) -> gtk::Overlay {
    let tile = gtk::DrawingArea::builder().content_width(24).content_height(24).build();
    tile.set_draw_func(move |_, cr, w, h| {
        v3::draw_round_rect(cr, 0.0, 0.0, f64::from(w), f64::from(h), 6.0);
        cr.set_source_rgba(rgb.0, rgb.1, rgb.2, 0.22);
        let _ = cr.fill();
    });
    let overlay = gtk::Overlay::builder().child(&tile).valign(gtk::Align::Center).build();
    let glyph = gtk::Image::from_icon_name(icon);
    glyph.set_pixel_size(12);
    overlay.add_overlay(&glyph);
    overlay
}

impl MapView {
    pub fn new(bus: Rc<HoverBus>, on_drill: Rc<dyn Fn(Drill)>) -> Rc<Self> {
        let panel = v3::panel("Folder map");
        let meta = v3::meta("");
        meta.add_css_class("numeric");
        panel.trailing.append(&meta);
        let mode = v3::Segmented::new(&["Map", "Tree"], 0);
        mode.root
            .set_tooltip_text(Some("Map traces where files go; Tree shows the folders now and after side by side"));
        panel.trailing.append(&mode.root);
        let body = v3::vbox(8);
        body.add_css_class("v3-panel-pad");
        let hint = v3::meta("Hover a folder or ribbon to trace its files. Click to see the exact list.");
        body.append(&hint);

        // ── Map ────────────────────────────────────────────────────────────
        let map = v3::vbox(8);
        let header = v3::hbox(8);
        let from_count = v3::label("", &["v3-map-head"]);
        from_count.set_size_request(NODE_WIDTH as i32, -1);
        header.append(&from_count);
        let arrow = v3::label("→", &["tone-gold"]);
        arrow.set_hexpand(true);
        arrow.set_xalign(0.5);
        header.append(&arrow);
        let to_count = v3::label("", &["v3-map-head"]);
        to_count.set_size_request(NODE_WIDTH as i32, -1);
        to_count.set_xalign(1.0);
        header.append(&to_count);
        map.append(&header);
        let ribbons = gtk::DrawingArea::builder().hexpand(true).content_height(320).build();
        let nodes = gtk::Fixed::new();
        let overlay = gtk::Overlay::builder().child(&ribbons).build();
        overlay.add_overlay(&nodes);
        map.append(&overlay);

        // ── Tree ───────────────────────────────────────────────────────────
        let tree = v3::vbox(10);
        let tree_head = v3::hbox(8);
        tree_head.append(&v3::label("Show", &["v3-output-label"]));
        let filter = v3::Segmented::new(&["All", "Moves", "New folders"], 0);
        tree_head.append(&filter.root);
        tree_head.append(&v3::spacer());
        for (letter, tone, label) in [("=", Tone::Done, "Unchanged"), ("M", Tone::Warning, "Moved"), ("+", Tone::Gold, "New folder")] {
            tree_head.append(&letter_badge(letter, tone));
            tree_head.append(&v3::label(label, &["v3-meta-115"]));
        }
        tree.append(&tree_head);
        let panes = gtk::Box::builder().homogeneous(true).spacing(12).build();
        let (today_pane, today) = tree_pane("Today", "Where files live now");
        let (proposed_pane, proposed) = tree_pane("Proposed", "Where they'll live");
        panes.append(&today_pane);
        panes.append(&proposed_pane);
        tree.append(&panes);

        let stack = gtk::Stack::builder()
            .vhomogeneous(false)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(if crate::spring::animations_enabled() { 180 } else { 0 })
            .build();
        stack.add_named(&map, Some("map"));
        stack.add_named(&tree, Some("tree"));
        body.append(&stack);
        panel.body.append(&body);
        let root = panel.root.clone();

        let view = Rc::new(Self {
            root,
            panel,
            meta,
            hint,
            stack,
            ribbons,
            nodes,
            overlay,
            from_count,
            to_count,
            layout: Rc::new(RefCell::new(MapLayout::default())),
            buttons: RefCell::new(Vec::new()),
            height: Rc::new(Cell::new(320.0)),
            entrance: RefCell::new(None),
            filter,
            today,
            proposed,
            items: RefCell::new(Vec::new()),
            staying: RefCell::new(Vec::new()),
            bus,
            on_drill,
        });
        view.wire(&mode);
        view
    }

    fn wire(self: &Rc<Self>, mode: &Rc<v3::Segmented>) {
        let weak = Rc::downgrade(self);
        mode.connect_changed(move |index| {
            let Some(view) = weak.upgrade() else { return };
            let tree = index == 1;
            view.stack.set_visible_child_name(if tree { "tree" } else { "map" });
            view.panel.set_title(if tree { "Now → After" } else { "Folder map" });
            view.hint.set_text(if tree {
                "Click any folder to see its files."
            } else {
                "Hover a folder or ribbon to trace its files. Click to see the exact list."
            });
        });
        let weak = Rc::downgrade(self);
        self.filter.connect_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.render_tree();
            }
        });
        let state = self.layout.clone();
        let bus = Rc::downgrade(&self.bus);
        let draw_height = self.height.clone();
        self.ribbons.set_draw_func(move |_, cr, width, _| {
            let hover = bus.upgrade().and_then(|bus| bus.current());
            draw_ribbons(cr, &state.borrow(), f64::from(width), draw_height.get(), hover.as_ref());
        });
        let weak = Rc::downgrade(self);
        self.ribbons.connect_resize(move |_, _, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(view) = weak.upgrade() {
                    view.place_nodes();
                }
            });
        });
        let weak = Rc::downgrade(self);
        self.bus.listen("map", move |_| {
            if let Some(view) = weak.upgrade() {
                view.ribbons.queue_draw();
            }
        });
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            if let Some(view) = weak.upgrade() {
                view.hover_ribbon(x, y);
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            let Some(view) = weak.upgrade() else { return };
            if matches!(view.bus.current(), Some(Hover::Flow(..))) {
                view.bus.set(None);
            }
            view.overlay.set_tooltip_text(None);
        });
        self.overlay.add_controller(motion);
    }

    /// The plan's moves and the folders that keep all their files
    /// (full path, display path, files).
    pub fn set_data(self: &Rc<Self>, items: Vec<MapItem>, staying: Vec<(String, String, u64)>) {
        if *self.items.borrow() == items && *self.staying.borrow() == staying && !items.is_empty() {
            return;
        }
        let layout = compute(&items);
        let sources: BTreeSet<&str> = items.iter().map(|i| i.source_folder.as_str()).collect();
        let dests: BTreeSet<&str> = items.iter().map(|i| i.bucket.as_str()).collect();
        self.meta.set_text(&format!(
            "{} → {}",
            super::restructure_model::plural(sources.len(), "source"),
            super::restructure_model::plural(dests.len(), "destination")
        ));
        self.from_count.set_text(&format!("FROM  {}", layout.sources.len()));
        self.to_count.set_text(&format!("{}  TO", layout.dests.len()));
        let height = map_height(&layout);
        self.height.set(height);
        self.ribbons.set_content_height(height as i32);
        self.layout.replace(layout);
        self.items.replace(items);
        self.staying.replace(staying);
        self.root.set_visible(!self.items.borrow().is_empty());
        self.build_nodes();
        self.place_nodes();
        self.render_tree();
        self.bus.set(None);
        let ribbons = self.ribbons.clone();
        if let Some(previous) = self.entrance.take() {
            previous.pause();
        }
        let animation = crate::spring::animate(&self.ribbons, 0.0, 1.0, move |value| {
            ribbons.set_opacity(value.clamp(0.0, 1.0));
        });
        self.entrance.replace(Some(animation));
        self.ribbons.queue_draw();
    }

    fn build_nodes(self: &Rc<Self>) {
        while let Some(child) = self.nodes.first_child() {
            self.nodes.remove(&child);
        }
        let layout = self.layout.borrow().clone();
        let mut buttons = Vec::new();
        for node in layout.sources.iter().chain(layout.dests.iter()) {
            let content = v3::hbox(8);
            let icon = if node.is_rollup() {
                "view-more-horizontal-symbolic"
            } else if node.source && node.junk {
                "folder-templates-symbolic"
            } else {
                "folder-symbolic"
            };
            content.append(&swatch(tint_rgb(node.tint), icon));
            let text = v3::vbox(0);
            text.set_valign(gtk::Align::Center);
            let name = v3::label(&node.label, &["v3-map-node-name"]);
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            if node.is_rollup() {
                name.add_css_class("tone-muted");
            }
            text.append(&name);
            text.append(&v3::label(&super::restructure_model::plural(node.count, "file"), &["v3-map-node-count"]));
            content.append(&text);
            let button = gtk::Button::builder().child(&content).css_classes(["v3-map-node"]).build();
            let spoken = format!("{} {}", if node.source { "From" } else { "To" }, node.label);
            button.update_property(&[
                gtk::accessible::Property::Label(&spoken),
                gtk::accessible::Property::Description(if node.source {
                    "Shows the files moving out of this folder"
                } else {
                    "Shows the files moving into this folder"
                }),
            ]);
            button.set_tooltip_text(Some(&format!(
                "{} · click to see them",
                super::restructure_model::plural(node.count, "file")
            )));
            let drill = self.on_drill.clone();
            let target = match (node.source, node.is_rollup()) {
                (true, true) => Drill::Sources(layout.rollup_sources.clone()),
                (true, false) => Drill::Source(node.key.clone()),
                (false, true) => Drill::Buckets(layout.rollup_dests.clone()),
                (false, false) => Drill::Bucket(node.key.clone()),
            };
            button.connect_clicked(move |_| drill(target.clone()));
            if !node.is_rollup() {
                let enter = if node.source { Hover::Source(node.key.clone()) } else { Hover::Bucket(node.key.clone()) };
                let lit_layout = self.layout.clone();
                let lit_node = node.clone();
                self.bus.track(&button, enter, move |hover| node_lit(&lit_layout.borrow(), &lit_node, Some(hover)));
            }
            self.nodes.put(&button, 0.0, 0.0);
            buttons.push((node.clone(), button));
        }
        self.buttons.replace(buttons);
    }

    fn place_nodes(&self) {
        let width = f64::from(self.ribbons.width());
        if width <= 0.0 {
            return;
        }
        let layout = self.layout.borrow();
        let height = self.height.get();
        let source_slots = slots(&layout.sources, height);
        let dest_slots = slots(&layout.dests, height);
        for (node, button) in self.buttons.borrow().iter() {
            let (column, slots_for) = if node.source {
                (&layout.sources, &source_slots)
            } else {
                (&layout.dests, &dest_slots)
            };
            let Some(index) = column.iter().position(|n| n.id == node.id) else { continue };
            let Some(&(top, h)) = slots_for.get(index) else { continue };
            let x = if node.source { 0.0 } else { (width - NODE_WIDTH).max(0.0) };
            button.set_size_request(NODE_WIDTH as i32, h.max(28.0) as i32);
            self.nodes.move_(button, x, top);
        }
    }

    fn hover_ribbon(&self, x: f64, y: f64) {
        let width = f64::from(self.ribbons.width());
        let (x0, x1) = (NODE_WIDTH, width - NODE_WIDTH);
        if x < x0 || x > x1 {
            if matches!(self.bus.current(), Some(Hover::Flow(..))) {
                self.bus.set(None);
            }
            return;
        }
        let layout = self.layout.borrow().clone();
        let height = self.height.get();
        let source_slots = slots(&layout.sources, height);
        let dest_slots = slots(&layout.dests, height);
        let mid = |nodes: &[Node], slots: &[(f64, f64)], id: &str| {
            nodes.iter().position(|n| n.id == id).and_then(|i| slots.get(i)).map(|(top, h)| top + h / 2.0)
        };
        let mut best: Option<(f64, &Flow)> = None;
        for flow in &layout.flows {
            let (Some(y0), Some(y1)) =
                (mid(&layout.sources, &source_slots, &flow.source_id), mid(&layout.dests, &dest_slots, &flow.dest_id))
            else {
                continue;
            };
            let dy = (bezier_y(x, x0, x1, y0, y1) - y).abs();
            let reach = ribbon_thickness(flow.count, layout.total, height) * 0.5 + 6.0;
            if dy < reach && best.map_or(true, |(closest, _)| dy < closest) {
                best = Some((dy, flow));
            }
        }
        match best {
            Some((_, flow)) => {
                let source = std::path::Path::new(&flow.source_folder)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.overlay.set_tooltip_text(Some(&format!(
                    "{source} → {}\n{}",
                    flow.bucket,
                    super::restructure_model::plural(flow.count, "file")
                )));
                self.bus.set(Some(Hover::Flow(flow.source_folder.clone(), flow.bucket.clone())));
            }
            None => {
                self.overlay.set_tooltip_text(None);
                if matches!(self.bus.current(), Some(Hover::Flow(..))) {
                    self.bus.set(None);
                }
            }
        }
    }

    fn render_tree(self: &Rc<Self>) {
        v3::clear(&self.today);
        v3::clear(&self.proposed);
        let filter = self.filter.selected();
        let items = self.items.borrow();
        let staying = self.staying.borrow();
        let mut moving: BTreeMap<&str, (usize, bool)> = BTreeMap::new();
        let mut buckets: BTreeMap<&str, usize> = BTreeMap::new();
        for item in items.iter() {
            let entry = moving.entry(&item.source_folder).or_insert((0, true));
            entry.0 += 1;
            entry.1 &= item.outcome == Outcome::Reorganize;
            *buckets.entry(&item.bucket).or_default() += 1;
        }
        let mut sources: Vec<(String, String, u64, Option<bool>)> = Vec::new();
        if filter == 0 {
            sources.extend(staying.iter().map(|(full, shown, count)| (full.clone(), shown.clone(), *count, None)));
        }
        for (folder, (count, junk)) in &moving {
            if filter == 2 && !junk {
                continue;
            }
            let shown = std::path::Path::new(folder)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.to_string());
            sources.push((folder.to_string(), shown, *count as u64, Some(*junk)));
        }
        let mut dests: Vec<(String, u64, bool)> = buckets.iter().map(|(b, c)| (b.to_string(), *c as u64, false)).collect();
        if filter == 0 {
            dests.splice(0..0, staying.iter().map(|(_, shown, count)| (shown.clone(), *count, true)));
        }
        for (full, shown, count, kind) in sources.iter().take(TREE_ROW_CAP) {
            let (letter, tone) = match kind {
                None => ("=", Tone::Done),
                Some(false) => ("M", Tone::Warning),
                Some(true) => ("M", Tone::Gold),
            };
            let tip = match kind {
                None => format!("Stays put: {} unchanged.", super::restructure_model::plural(*count as usize, "file")),
                Some(false) => format!("Tidy: {} moving out.", super::restructure_model::plural(*count as usize, "file")),
                Some(true) => format!("Reorganize: all {} moving.", super::restructure_model::plural(*count as usize, "file")),
            };
            let row = self.tree_row(letter, tone, shown, *count, &tip, kind.map(|_| Drill::Source(full.clone())));
            let folder = full.clone();
            let lit = folder.clone();
            self.bus.track(&row, Hover::Source(folder), move |hover| hover.touches_source(&lit));
            self.today.append(&row);
        }
        if sources.len() > TREE_ROW_CAP {
            self.today.append(&v3::meta(&format!("+ {} more", sources.len() - TREE_ROW_CAP)));
        }
        for (bucket, count, existing) in dests.iter().take(TREE_ROW_CAP) {
            let (letter, tone) = if *existing { ("=", Tone::Done) } else { ("+", Tone::Gold) };
            let tip = if *existing {
                format!("Existing folder: {} staying.", super::restructure_model::plural(*count as usize, "file"))
            } else {
                format!("New folder: {} landing here.", super::restructure_model::plural(*count as usize, "file"))
            };
            let row = self.tree_row(letter, tone, bucket, *count, &tip, (!existing).then(|| Drill::Bucket(bucket.clone())));
            let lit = bucket.clone();
            self.bus.track(&row, Hover::Bucket(bucket.clone()), move |hover| hover.touches_bucket(&lit));
            self.proposed.append(&row);
        }
        if dests.len() > TREE_ROW_CAP {
            self.proposed.append(&v3::meta(&format!("+ {} more", dests.len() - TREE_ROW_CAP)));
        }
    }

    fn tree_row(&self, letter: &str, tone: Tone, name: &str, count: u64, tip: &str, drill: Option<Drill>) -> gtk::Widget {
        let content = v3::hbox(6);
        content.append(&letter_badge(letter, tone));
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.set_pixel_size(11);
        icon.add_css_class(tone.css());
        content.append(&icon);
        let label = v3::mono(name, "s11");
        label.set_hexpand(true);
        content.append(&label);
        let number = v3::label(&crate::shell::grouped(count), &["v3-snippet", "numeric"]);
        content.append(&number);
        match drill {
            Some(drill) => {
                let button = gtk::Button::builder().child(&content).css_classes(["v3-tree-row", tone.css()]).build();
                button.set_tooltip_text(Some(tip));
                let on_drill = self.on_drill.clone();
                button.connect_clicked(move |_| on_drill(drill.clone()));
                button.upcast()
            }
            None => {
                content.add_css_class("v3-tree-row");
                content.add_css_class(tone.css());
                content.set_tooltip_text(Some(tip));
                content.upcast()
            }
        }
    }
}

fn letter_badge(letter: &str, tone: Tone) -> gtk::Label {
    let badge = v3::label(letter, &["v3-letter", tone.css()]);
    badge.set_xalign(0.5);
    badge.set_valign(gtk::Align::Center);
    badge
}

fn tree_pane(title: &str, subtitle: &str) -> (gtk::Box, gtk::Box) {
    let pane = v3::vbox(6);
    pane.add_css_class("v3-tree-pane");
    let head = v3::vbox(1);
    head.append(&v3::label(title, &["v3-title-12"]));
    head.append(&v3::label(subtitle, &["v3-meta-115"]));
    pane.append(&head);
    let rows = v3::vbox(4);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(280)
        .max_content_height(420)
        .propagate_natural_height(true)
        .child(&rows)
        .build();
    pane.append(&scroll);
    (pane, rows)
}

fn draw_ribbons(cr: &gtk::cairo::Context, layout: &MapLayout, width: f64, height: f64, hover: Option<&Hover>) {
    if layout.flows.is_empty() || width <= NODE_WIDTH * 2.0 {
        return;
    }
    let (x0, x1) = (NODE_WIDTH, width - NODE_WIDTH);
    let source_slots = slots(&layout.sources, height);
    let dest_slots = slots(&layout.dests, height);
    let mid = |nodes: &[Node], slots: &[(f64, f64)], id: &str| {
        nodes.iter().position(|n| n.id == id).and_then(|i| slots.get(i)).map(|(top, h)| top + h / 2.0)
    };
    let alpha = |flow: &Flow| -> f64 {
        match hover {
            None => 0.18,
            Some(Hover::Flow(folder, bucket)) => {
                if flow.source_folder == *folder && flow.bucket == *bucket {
                    0.95
                } else {
                    0.04
                }
            }
            Some(Hover::Source(folder)) => {
                if flow.source_folder == *folder {
                    0.6
                } else {
                    0.04
                }
            }
            Some(Hover::Bucket(bucket)) => {
                if flow.bucket == *bucket {
                    0.6
                } else {
                    0.04
                }
            }
            Some(Hover::Outcome(outcome)) => {
                let lit = layout.nodes_by_outcome.get(outcome);
                if lit.is_some_and(|set| set.contains(&flow.source_id) && set.contains(&flow.dest_id)) {
                    0.45
                } else {
                    0.04
                }
            }
        }
    };
    let mut ordered: Vec<&Flow> = layout.flows.iter().collect();
    ordered.sort_by(|a, b| alpha(a).total_cmp(&alpha(b)));
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    for flow in ordered {
        let (Some(y0), Some(y1)) =
            (mid(&layout.sources, &source_slots, &flow.source_id), mid(&layout.dests, &dest_slots, &flow.dest_id))
        else {
            continue;
        };
        let (r, g, b) = tint_rgb(flow.tint);
        let mid_x = (x0 + x1) / 2.0;
        cr.move_to(x0, y0);
        cr.curve_to(mid_x, y0, mid_x, y1, x1, y1);
        cr.set_source_rgba(r, g, b, alpha(flow));
        cr.set_line_width(ribbon_thickness(flow.count, layout.total, height));
        let _ = cr.stroke();
    }
}
