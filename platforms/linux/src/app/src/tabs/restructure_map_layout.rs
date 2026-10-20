// Folder-map layout (macOS `SankeyFlowView.computeLayout` / `layoutSlots`):
// top eight sources and destinations with "+ N more" rollups, aggregated
// flows, two barycentric passes to cut crossings, proportional slots, and
// the hover bus shared by the map, tree, recommendations and Staying put.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use super::restructure_model::Outcome;

pub const TOP_N: usize = 8;
pub const OTHER_SOURCE: &str = "src:__other__";
pub const OTHER_DEST: &str = "dst:__other__";
const GAP: f64 = 14.0;
const BUFFER: f64 = 14.0;
const PREFERRED_MIN: f64 = 28.0;
const ABSOLUTE_MIN: f64 = 18.0;

/// Okabe-Ito colour-blind-safe palette for destinations (RESTRUCTURE.md §7).
pub const PALETTE: [(f64, f64, f64); 7] = [
    (0.000, 0.447, 0.698),
    (0.835, 0.369, 0.000),
    (0.000, 0.620, 0.451),
    (0.337, 0.706, 0.914),
    (0.902, 0.624, 0.000),
    (0.800, 0.475, 0.655),
    (0.941, 0.894, 0.259),
];

#[derive(Debug, Clone, PartialEq)]
pub struct MapItem {
    /// Full path of the folder the file leaves.
    pub source_folder: String,
    /// Destination folder relative to the library root.
    pub bucket: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub key: String,
    pub count: usize,
    pub tint: Option<usize>,
    pub source: bool,
    pub junk: bool,
}

impl Node {
    pub fn is_rollup(&self) -> bool {
        self.id == OTHER_SOURCE || self.id == OTHER_DEST
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub source_id: String,
    pub dest_id: String,
    pub source_folder: String,
    pub bucket: String,
    pub tint: Option<usize>,
    pub count: usize,
}

#[derive(Debug, Default, Clone)]
pub struct MapLayout {
    pub sources: Vec<Node>,
    pub dests: Vec<Node>,
    pub flows: Vec<Flow>,
    pub total: usize,
    pub dests_for_source: HashMap<String, HashSet<String>>,
    pub sources_for_dest: HashMap<String, HashSet<String>>,
    pub nodes_by_outcome: HashMap<Outcome, HashSet<String>>,
    pub rollup_sources: Vec<String>,
    pub rollup_dests: Vec<String>,
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

pub fn compute(items: &[MapItem]) -> MapLayout {
    if items.is_empty() {
        return MapLayout::default();
    }
    let mut source_counts: HashMap<&str, (usize, bool)> = HashMap::new();
    let mut dest_counts: HashMap<&str, usize> = HashMap::new();
    for item in items {
        let entry = source_counts.entry(&item.source_folder).or_insert((0, true));
        entry.0 += 1;
        entry.1 &= item.outcome == Outcome::Reorganize;
        *dest_counts.entry(&item.bucket).or_default() += 1;
    }
    let mut all_sources: Vec<(&str, usize, bool)> =
        source_counts.into_iter().map(|(folder, (count, junk))| (folder, count, junk)).collect();
    all_sources.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut all_dests: Vec<(&str, usize)> = dest_counts.into_iter().collect();
    all_dests.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let (visible_sources, tail_sources) = all_sources.split_at(all_sources.len().min(TOP_N));
    let (visible_dests, tail_dests) = all_dests.split_at(all_dests.len().min(TOP_N));

    let mut sources: Vec<Node> = visible_sources
        .iter()
        .map(|(folder, count, junk)| Node {
            id: format!("src:{folder}"),
            label: basename(folder),
            key: (*folder).to_owned(),
            count: *count,
            tint: None,
            source: true,
            junk: *junk,
        })
        .collect();
    if !tail_sources.is_empty() {
        sources.push(Node {
            id: OTHER_SOURCE.into(),
            label: format!("+ {} more", plural(tail_sources.len(), "folder")),
            key: OTHER_SOURCE.into(),
            count: tail_sources.iter().map(|s| s.1).sum(),
            tint: None,
            source: true,
            junk: false,
        });
    }
    let mut dests: Vec<Node> = visible_dests
        .iter()
        .enumerate()
        .map(|(index, (bucket, count))| Node {
            id: format!("dst:{bucket}"),
            label: (*bucket).to_owned(),
            key: (*bucket).to_owned(),
            count: *count,
            tint: Some(index % PALETTE.len()),
            source: false,
            junk: false,
        })
        .collect();
    if !tail_dests.is_empty() {
        dests.push(Node {
            id: OTHER_DEST.into(),
            label: format!("+ {} more", plural(tail_dests.len(), "bucket")),
            key: OTHER_DEST.into(),
            count: tail_dests.iter().map(|d| d.1).sum(),
            tint: None,
            source: false,
            junk: false,
        });
    }

    let visible_source_ids: HashSet<String> = sources.iter().map(|n| n.id.clone()).collect();
    let visible_dest_ids: HashSet<String> = dests.iter().map(|n| n.id.clone()).collect();
    let tint_by_dest: HashMap<String, Option<usize>> = dests.iter().map(|n| (n.id.clone(), n.tint)).collect();
    let source_id = |folder: &str| {
        let id = format!("src:{folder}");
        if visible_source_ids.contains(&id) {
            id
        } else {
            OTHER_SOURCE.to_owned()
        }
    };
    let dest_id = |bucket: &str| {
        let id = format!("dst:{bucket}");
        if visible_dest_ids.contains(&id) {
            id
        } else {
            OTHER_DEST.to_owned()
        }
    };
    let mut flows: Vec<Flow> = Vec::new();
    let mut flow_index: HashMap<(String, String), usize> = HashMap::new();
    let mut dests_for_source: HashMap<String, HashSet<String>> = HashMap::new();
    let mut sources_for_dest: HashMap<String, HashSet<String>> = HashMap::new();
    let mut nodes_by_outcome: HashMap<Outcome, HashSet<String>> = HashMap::new();
    for item in items {
        let (src, dst) = (source_id(&item.source_folder), dest_id(&item.bucket));
        match flow_index.get(&(src.clone(), dst.clone())) {
            Some(&index) => flows[index].count += 1,
            None => {
                flow_index.insert((src.clone(), dst.clone()), flows.len());
                flows.push(Flow {
                    source_id: src.clone(),
                    dest_id: dst.clone(),
                    source_folder: item.source_folder.clone(),
                    bucket: item.bucket.clone(),
                    tint: tint_by_dest.get(&dst).copied().flatten(),
                    count: 1,
                });
            }
        }
        dests_for_source.entry(item.source_folder.clone()).or_default().insert(item.bucket.clone());
        sources_for_dest.entry(item.bucket.clone()).or_default().insert(item.source_folder.clone());
        let lit = nodes_by_outcome.entry(item.outcome).or_default();
        lit.insert(src);
        lit.insert(dst);
    }
    let total = flows.iter().map(|f| f.count).sum();

    for _ in 0..2 {
        let dest_index: HashMap<&str, usize> = dests.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let weight = |id: &str, by_source: bool, index: &HashMap<&str, usize>| -> f64 {
            let (sum, weighted) = flows
                .iter()
                .filter(|f| if by_source { f.source_id == id } else { f.dest_id == id })
                .fold((0usize, 0.0), |(sum, weighted), f| {
                    let other = if by_source { &f.dest_id } else { &f.source_id };
                    (sum + f.count, weighted + f.count as f64 * *index.get(other.as_str()).unwrap_or(&0) as f64)
                });
            if sum == 0 {
                0.0
            } else {
                weighted / sum as f64
            }
        };
        let mut keyed: Vec<(f64, Node)> =
            sources.drain(..).map(|n| (weight(&n.id, true, &dest_index), n)).collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        sources = keyed.into_iter().map(|(_, n)| n).collect();
        let source_index: HashMap<&str, usize> =
            sources.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut keyed: Vec<(f64, Node)> =
            dests.drain(..).map(|n| (weight(&n.id, false, &source_index), n)).collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        dests = keyed.into_iter().map(|(_, n)| n).collect();
    }
    for column in [&mut sources, &mut dests] {
        if let Some(index) = column.iter().position(Node::is_rollup) {
            let rollup = column.remove(index);
            column.push(rollup);
        }
    }

    MapLayout {
        sources,
        dests,
        flows,
        total,
        dests_for_source,
        sources_for_dest,
        nodes_by_outcome,
        rollup_sources: tail_sources.iter().map(|s| s.0.to_owned()).collect(),
        rollup_dests: tail_dests.iter().map(|d| d.0.to_owned()).collect(),
    }
}

/// (top, height) per node: small nodes flex up to 28, the rest share the
/// column in proportion to their files, 14 apart with a 14 buffer.
pub fn slots(nodes: &[Node], total_height: f64) -> Vec<(f64, f64)> {
    if nodes.is_empty() || total_height <= 0.0 {
        return Vec::new();
    }
    let n = nodes.len() as f64;
    let available = ((total_height - BUFFER * 2.0).max(0.0) - (n - 1.0) * GAP).max(0.0);
    let mut heights: Vec<f64>;
    if available < ABSOLUTE_MIN * n {
        heights = vec![(available / n).max(0.0); nodes.len()];
    } else {
        let total: usize = nodes.iter().map(|n| n.count).sum();
        let threshold = (total / 20).max(1);
        heights = vec![0.0; nodes.len()];
        let mut fixed = 0.0;
        let mut flex_sum = 0usize;
        for (i, node) in nodes.iter().enumerate() {
            if node.count <= threshold {
                heights[i] = PREFERRED_MIN;
                fixed += PREFERRED_MIN;
            } else {
                flex_sum += node.count;
            }
        }
        let flex = (available - fixed).max(0.0);
        for (i, node) in nodes.iter().enumerate() {
            if heights[i] == 0.0 {
                heights[i] = if flex_sum > 0 {
                    (flex * node.count as f64 / flex_sum as f64).max(PREFERRED_MIN)
                } else {
                    PREFERRED_MIN
                };
            }
        }
        let sum: f64 = heights.iter().sum();
        if sum > available {
            let scale = available / sum;
            for h in &mut heights {
                *h = (*h * scale).max(ABSOLUTE_MIN);
            }
            if heights.iter().sum::<f64>() > available {
                heights = vec![available / n; nodes.len()];
            }
        }
    }
    let mut y = BUFFER;
    heights
        .into_iter()
        .map(|h| {
            let slot = (y, h);
            y += h + GAP;
            slot
        })
        .collect()
}

pub fn map_height(layout: &MapLayout) -> f64 {
    let nodes = layout.sources.len().max(layout.dests.len()).max(4) as f64;
    (nodes * 56.0).clamp(320.0, 580.0)
}

pub fn ribbon_thickness(count: usize, total: usize, height: f64) -> f64 {
    (count as f64 / total.max(1) as f64 * height * 0.5).clamp(1.5, 9.0)
}

/// Y of the ribbon at `x` on the cubic from (x0, y0) to (x1, y1) whose
/// control points sit halfway across at the end heights.
pub fn bezier_y(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    let dx = x1 - x0;
    if dx.abs() < f64::EPSILON {
        return y0;
    }
    let curve_x = |t: f64| {
        let u = 1.0 - t;
        x0 + dx * (3.0 * u * u * t * 0.5 + 3.0 * u * t * t * 0.5 + t * t * t)
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..32 {
        let mid = (lo + hi) / 2.0;
        if curve_x(mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = (lo + hi) / 2.0;
    let u = 1.0 - t;
    y0 * (u * u * u + 3.0 * u * u * t) + y1 * (3.0 * u * t * t + t * t * t)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hover {
    Source(String),
    Bucket(String),
    Outcome(Outcome),
    Flow(String, String),
}

impl Hover {
    pub fn touches_source(&self, folder: &str) -> bool {
        matches!(self, Hover::Source(f) | Hover::Flow(f, _) if f == folder)
    }

    pub fn touches_bucket(&self, bucket: &str) -> bool {
        matches!(self, Hover::Bucket(b) | Hover::Flow(_, b) if b == bucket)
    }
}

/// Node focus under the current hover (macOS `isNodeFocused`).
pub fn node_lit(layout: &MapLayout, node: &Node, hover: Option<&Hover>) -> bool {
    match hover {
        None => false,
        Some(Hover::Source(folder)) => {
            if node.source {
                node.key == *folder
            } else {
                layout.dests_for_source.get(folder).is_some_and(|set| set.contains(&node.key))
            }
        }
        Some(Hover::Bucket(bucket)) => {
            if node.source {
                layout.sources_for_dest.get(bucket).is_some_and(|set| set.contains(&node.key))
            } else {
                node.key == *bucket
            }
        }
        Some(Hover::Flow(folder, bucket)) => {
            if node.source {
                node.key == *folder
            } else {
                node.key == *bucket
            }
        }
        Some(Hover::Outcome(outcome)) => layout.nodes_by_outcome.get(outcome).is_some_and(|set| set.contains(&node.id)),
    }
}

type Listener = Rc<dyn Fn(Option<&Hover>)>;

/// Shared hover state. Widgets register a predicate and get the `lit` class
/// while the hover touches them; views register listeners to redraw.
#[derive(Default)]
pub struct HoverBus {
    current: RefCell<Option<Hover>>,
    tracked: RefCell<Vec<(gtk::glib::WeakRef<gtk::Widget>, Rc<dyn Fn(&Hover) -> bool>)>>,
    listeners: RefCell<HashMap<&'static str, Listener>>,
}

impl HoverBus {
    pub fn current(&self) -> Option<Hover> {
        self.current.borrow().clone()
    }

    pub fn set(&self, hover: Option<Hover>) {
        if *self.current.borrow() == hover {
            return;
        }
        self.current.replace(hover.clone());
        use gtk::prelude::*;
        self.tracked.borrow_mut().retain(|(widget, _)| widget.upgrade().is_some());
        let tracked: Vec<_> = self.tracked.borrow().iter().map(|(w, p)| (w.clone(), p.clone())).collect();
        for (widget, predicate) in tracked {
            if let Some(widget) = widget.upgrade() {
                if hover.as_ref().is_some_and(|h| predicate(h)) {
                    widget.add_css_class("lit");
                } else {
                    widget.remove_css_class("lit");
                }
            }
        }
        let listeners: Vec<Listener> = self.listeners.borrow().values().cloned().collect();
        for listener in listeners {
            listener(hover.as_ref());
        }
    }

    /// Tracks `widget` and sets the hover while the pointer is over it.
    pub fn track(
        self: &Rc<Self>,
        widget: &impl gtk::prelude::IsA<gtk::Widget>,
        on_enter: Hover,
        lit: impl Fn(&Hover) -> bool + 'static,
    ) {
        use gtk::prelude::*;
        self.tracked.borrow_mut().push((widget.upcast_ref::<gtk::Widget>().downgrade(), Rc::new(lit)));
        let motion = gtk::EventControllerMotion::new();
        let bus: Weak<Self> = Rc::downgrade(self);
        let enter = on_enter.clone();
        motion.connect_enter(move |_, _, _| {
            if let Some(bus) = bus.upgrade() {
                bus.set(Some(enter.clone()));
            }
        });
        let bus: Weak<Self> = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(bus) = bus.upgrade() {
                if bus.current().as_ref() == Some(&on_enter) {
                    bus.set(None);
                }
            }
        });
        widget.add_controller(motion);
    }

    pub fn listen(&self, name: &'static str, listener: impl Fn(Option<&Hover>) + 'static) {
        self.listeners.borrow_mut().insert(name, Rc::new(listener));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(source: &str, bucket: &str, outcome: Outcome) -> MapItem {
        MapItem { source_folder: source.into(), bucket: bucket.into(), outcome }
    }

    #[test]
    fn flows_aggregate_and_long_tails_roll_up() {
        let mut items = vec![
            item("/l/Untitled", "Photos/2024", Outcome::Reorganize),
            item("/l/Untitled", "Photos/2024", Outcome::Reorganize),
            item("/l/Trips", "Documents", Outcome::Tidy),
        ];
        for i in 0..10 {
            items.push(item(&format!("/l/f{i}"), &format!("B{i}"), Outcome::Reorganize));
        }
        let layout = compute(&items);
        assert_eq!(layout.total, 13);
        assert_eq!(layout.sources.len(), TOP_N + 1);
        assert_eq!(layout.dests.len(), TOP_N + 1);
        assert!(layout.sources.last().unwrap().is_rollup());
        assert!(layout.dests.last().unwrap().is_rollup());
        assert_eq!(layout.rollup_sources.len(), 12 - TOP_N);
        let untitled = layout.sources.iter().find(|n| n.key == "/l/Untitled").unwrap();
        assert_eq!((untitled.label.as_str(), untitled.count, untitled.junk), ("Untitled", 2, true));
        assert!(!layout.sources.iter().find(|n| n.key == "/l/Trips").unwrap().junk);
        let flow = layout.flows.iter().find(|f| f.source_folder == "/l/Untitled").unwrap();
        assert_eq!(flow.count, 2);
        assert!(flow.tint.is_some());
        let tidy = &layout.nodes_by_outcome[&Outcome::Tidy];
        assert!(tidy.contains("src:/l/Trips"));
        assert!(tidy.contains(OTHER_DEST), "Documents sorts into the rollup behind B0..B9");
    }

    #[test]
    fn barycentric_order_uncrosses_two_parallel_flows() {
        let items = vec![
            item("/l/A", "Zeta", Outcome::Tidy),
            item("/l/A", "Zeta", Outcome::Tidy),
            item("/l/B", "Alpha", Outcome::Tidy),
        ];
        let layout = compute(&items);
        let src: Vec<_> = layout.sources.iter().map(|n| n.key.as_str()).collect();
        let dst: Vec<_> = layout.dests.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(src.iter().position(|s| *s == "/l/A"), dst.iter().position(|d| *d == "Zeta"));
    }

    #[test]
    fn slots_fit_the_column_with_gaps() {
        let layout = compute(&[item("/l/A", "X", Outcome::Tidy), item("/l/B", "Y", Outcome::Tidy)]);
        let height = map_height(&layout);
        assert_eq!(height, 320.0);
        let slots = slots(&layout.sources, height);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].0, 14.0);
        assert!(slots[0].1 >= 28.0);
        let (top, h) = slots[1];
        assert!((top - (14.0 + slots[0].1 + 14.0)).abs() < 1e-9);
        assert!(top + h <= height - 14.0 + 1e-9);
    }

    #[test]
    fn bezier_hits_both_ends_and_the_midpoint() {
        assert!((bezier_y(0.0, 0.0, 100.0, 10.0, 90.0) - 10.0).abs() < 1e-6);
        assert!((bezier_y(100.0, 0.0, 100.0, 10.0, 90.0) - 90.0).abs() < 1e-6);
        assert!((bezier_y(50.0, 0.0, 100.0, 10.0, 90.0) - 50.0).abs() < 1e-6);
        assert_eq!(ribbon_thickness(1, 1000, 400.0), 1.5);
        assert_eq!(ribbon_thickness(1000, 1000, 400.0), 9.0);
    }

    #[test]
    fn hover_lights_connected_nodes() {
        let layout = compute(&[item("/l/A", "X", Outcome::Tidy), item("/l/B", "Y", Outcome::Reorganize)]);
        let a = layout.sources.iter().find(|n| n.key == "/l/A").unwrap();
        let x = layout.dests.iter().find(|n| n.key == "X").unwrap();
        let y = layout.dests.iter().find(|n| n.key == "Y").unwrap();
        let hover = Hover::Source("/l/A".into());
        assert!(node_lit(&layout, a, Some(&hover)));
        assert!(node_lit(&layout, x, Some(&hover)));
        assert!(!node_lit(&layout, y, Some(&hover)));
        assert!(node_lit(&layout, y, Some(&Hover::Outcome(Outcome::Reorganize))));
        assert!(Hover::Flow("/l/A".into(), "X".into()).touches_bucket("X"));
        assert!(!node_lit(&layout, a, None));
    }
}
