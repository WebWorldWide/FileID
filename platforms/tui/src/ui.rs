//! Rendering. Pure read of `&App` → ratatui widgets. No state mutation here.
//!
//! Honors the FileID signature palette (gold `#FFCC00`, lavender `#B19BCE`,
//! cyan `#A0E2EA`, pink `#F2A6C0`) at terminal fidelity. The desktop apps'
//! LavaLampBackground has no terminal analogue; the accent palette carries the
//! brand instead.
//!
//! FIX 1 — readable on ANY terminal theme: we paint our OWN brand-dark
//! background (`BG`) across the whole frame first, with a near-white default
//! foreground (`FG`); every widget below patches only its `fg`, inheriting the
//! dark `bg`. So a light-background terminal can't wash out the gold/light
//! accents — we never depend on the terminal's default colors anywhere visible
//! (overlays re-establish `BG` after `Clear`).

use std::rc::Rc;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Tab};
use crate::data::{human_size, short};

/// Brand near-black background painted under the entire UI so light terminal
/// themes can't render the gold/light foreground invisibly.
const BG: Color = Color::Rgb(18, 18, 20);
/// Near-white default body text — strong contrast on `BG`.
const FG: Color = Color::Rgb(229, 229, 234);
const GOLD: Color = Color::Rgb(255, 204, 0);
const LAVENDER: Color = Color::Rgb(177, 155, 206);
const CYAN: Color = Color::Rgb(160, 226, 234);
const PINK: Color = Color::Rgb(242, 166, 192);
const DIM: Color = Color::Rgb(140, 140, 150);

/// Top-level vertical split: tab bar (3) · body (rest) · status line (1).
/// Pure + deterministic so it can be unit-tested without a terminal.
pub fn frame_chunks(area: Rect) -> Rc<[Rect]> {
    Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)]).split(area)
}

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    // FIX 1: paint the brand-dark background under everything FIRST. A Block's
    // style fills the whole area's bg; subsequent widgets patch only fg, so the
    // dark bg shows through — legible on light AND dark terminals alike.
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);

    let chunks = frame_chunks(area);
    render_tabs(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_status(f, app, chunks[2]);
    if app.show_help {
        render_help(f, area);
    }
    if app.input_active {
        render_input(f, app, area);
    }
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!(" {}·{} ", i + 1, t.title())))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .style(Style::default().fg(LAVENDER))
        .highlight_style(Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD))
        .divider(Span::styled("│", Style::default().fg(DIM)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(LAVENDER))
                .title(Span::styled(" FileID ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        );
    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    if !app.data.db_exists && !app.loading {
        render_no_db(f, app, area);
        return;
    }
    match app.tab {
        Tab::Library => render_library(f, app, area),
        Tab::People => render_people(f, app, area),
        Tab::Cleanup => render_cleanup(f, app, area),
        Tab::Restructure => render_restructure(f, app, area),
        Tab::Settings => render_settings(f, app, area),
    }
}

fn render_no_db(f: &mut Frame, app: &App, area: Rect) {
    let text = Text::from(vec![
        Line::from(Span::styled("No library database found.", Style::default().fg(PINK).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(format!("Resolved path: {}", app.db_label)),
        Line::from(""),
        Line::from(Span::styled(
            "Press  s  to pick a folder and scan it with the engine,",
            Style::default().fg(GOLD),
        )),
        Line::from(Span::styled(
            "or index with the CLI:  fileid scan <path> --models",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled("…then reload here with  r", Style::default().fg(DIM))),
    ]);
    let p = Paragraph::new(text)
        .wrap(Wrap { trim: true })
        .block(titled_block("Library", LAVENDER));
    f.render_widget(p, area);
}

fn render_library(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    let visible = app.visible_files();
    let cursor = app.cursor();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|fr| {
            let kind = kind_span(&fr.kind);
            let line = Line::from(vec![
                kind,
                Span::raw(" "),
                Span::raw(basename(&fr.path)),
                Span::raw("  "),
                Span::styled(human_size(fr.size), Style::default().fg(DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = if app.search_active {
        format!("Library  /{}_", app.search)
    } else if !app.search.is_empty() {
        format!("Library  /{}  ({} match)", app.search, visible.len())
    } else if app.scanning {
        format!("Library  ⟳ scanning {}", app.scan_root.as_deref().map_or_else(String::new, short))
    } else if let Some(root) = &app.scan_root {
        format!("Library  ({} files) · scanned {}", visible.len(), short(root))
    } else {
        format!("Library  ({} files)", visible.len())
    };

    render_list(f, cols[0], &title, items, cursor, GOLD);
    render_file_detail(f, app, cols[1], visible.get(cursor).copied());
}

fn render_file_detail(f: &mut Frame, app: &App, area: Rect, file: Option<&crate::data::FileRow>) {
    let block = titled_block("Detail", CYAN);
    let Some(fr) = file else {
        let p = Paragraph::new(Span::styled("No file selected.", Style::default().fg(DIM)))
            .block(block);
        f.render_widget(p, area);
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(basename(&fr.path), Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(short(&fr.path), Style::default().fg(DIM))),
        Line::from(""),
        kv("kind", &format!("{} (.{})", fr.kind, fr.extension)),
        kv("size", &human_size(fr.size)),
        kv("modified", &fr.modified.map_or_else(|| "—".into(), fmt_date)),
        kv(
            "flags",
            &format!(
                "{}{}",
                if fr.has_text { "text " } else { "" },
                if fr.has_faces { "faces" } else { "" }
            ),
        ),
    ];

    if let Some(tags) = app.data.tags.get(&fr.id) {
        if !tags.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("tags", Style::default().fg(PINK).add_modifier(Modifier::BOLD))));
            lines.push(Line::from(Span::styled(tags.join(" · "), Style::default().fg(LAVENDER))));
        }
    }
    if let Some(snip) = app.data.snippets.get(&fr.id) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("text", Style::default().fg(PINK).add_modifier(Modifier::BOLD))));
        lines.push(Line::from(Span::styled(snip.clone(), Style::default().fg(DIM))));
    }

    let p = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }).block(block);
    f.render_widget(p, area);
}

fn render_people(f: &mut Frame, app: &App, area: Rect) {
    if app.data.people.is_empty() {
        render_empty(f, area, "People", "No person clusters yet.", "Faces come from a full engine scan with face models.");
        return;
    }
    let items: Vec<ListItem> = app
        .data
        .people
        .iter()
        .map(|p| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("#{:<4} ", p.id), Style::default().fg(DIM)),
                Span::styled(format!("{:<24}", truncate(&p.name, 24)), Style::default().fg(LAVENDER)),
                Span::styled(format!("{:>5} faces", p.faces), Style::default().fg(DIM)),
                Span::styled(format!("  {:>5} files", p.files), Style::default().fg(DIM)),
            ]))
        })
        .collect();
    render_list(f, area, &format!("People  ({} clusters)", app.data.people.len()), items, app.cursor(), GOLD);
}

fn render_cleanup(f: &mut Frame, app: &App, area: Rect) {
    if app.data.dupes.is_empty() {
        render_empty(f, area, "Cleanup", "No exact-duplicate groups.", "Content hashes come from a full engine scan.");
        return;
    }
    let cols = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let cursor = app.cursor();

    let items: Vec<ListItem> = app
        .data
        .dupes
        .iter()
        .map(|g| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} copies", g.paths.len()), Style::default().fg(PINK)),
                Span::raw("  "),
                Span::styled(human_size(g.size), Style::default().fg(DIM)),
                Span::raw("  "),
                Span::styled(format!("[{}]", &g.hash[..g.hash.len().min(10)]), Style::default().fg(DIM)),
            ]))
        })
        .collect();
    render_list(f, cols[0], &format!("Duplicate groups  ({})", app.data.dupes.len()), items, cursor, GOLD);

    // detail: files in the selected group
    let block = titled_block("Files in group", CYAN);
    let detail = match app.data.dupes.get(cursor) {
        Some(g) => {
            let lines: Vec<Line> = g
                .paths
                .iter()
                .map(|p| Line::from(Span::styled(short(p), Style::default().fg(LAVENDER))))
                .collect();
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true })
        }
        None => Paragraph::new(Span::styled("—", Style::default().fg(DIM))),
    };
    f.render_widget(detail.block(block), cols[1]);
}

fn render_restructure(f: &mut Frame, app: &App, area: Rect) {
    if app.data.plan.is_empty() {
        render_empty(f, area, "Restructure", "No proposed moves.", "Index some files, then reload (r) to preview a plan.");
        return;
    }
    let items: Vec<ListItem> = app
        .data
        .plan
        .iter()
        .map(|m| {
            ListItem::new(Line::from(vec![
                conf_span(m.confidence),
                Span::raw(" "),
                Span::styled(format!("{:<14}", truncate(&m.category, 14)), Style::default().fg(CYAN)),
                Span::raw(basename(&m.source)),
                Span::styled("  →  ", Style::default().fg(DIM)),
                Span::styled(rel_dest(&m.destination), Style::default().fg(LAVENDER)),
            ]))
        })
        .collect();
    render_list(
        f,
        area,
        &format!("Restructure plan  ({} moves · read-only preview)", app.data.plan.len()),
        items,
        app.cursor(),
        GOLD,
    );
}

fn render_settings(f: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::from(Span::styled("Library", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        kv("db path", &app.db_label),
        kv("exists", if app.data.db_exists { "yes" } else { "no" }),
        kv("files indexed", &app.data.total_files.to_string()),
        kv("tags", &app.data.total_tags.to_string()),
        kv("people", &app.data.people.len().to_string()),
        kv("duplicate groups", &app.data.dupes.len().to_string()),
        Line::from(""),
        Line::from(Span::styled("Engine", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        kv("read surface", "fileid_engine::db::open_read (in-process)"),
        kv("plan", "fileid_engine::pipeline::restructure::classify"),
        kv("scan", "engine-spawn startScan IPC + live events (press s)"),
        Line::from(""),
        Line::from(Span::styled("Stubbed (follow-on)", Style::default().fg(PINK).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("· face clustering via engine-spawn IPC", Style::default().fg(DIM))),
        Line::from(Span::styled("· semantic search, restructure apply, people merge", Style::default().fg(DIM))),
    ];
    let p = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }).block(titled_block("Settings", LAVENDER));
    f.render_widget(p, area);
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let prefix = if app.loading { "⏳ " } else { "✓ " };
    let hints = " [Tab]switch [↑↓/jk]move [/]search [s]scan [r]reload [?]help [q]quit";
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(if app.loading { PINK } else { CYAN })),
        Span::styled(truncate(&app.status, area.width.saturating_sub(hints.len() as u16 + 4) as usize), Style::default().fg(FG)),
        Span::styled(hints, Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let lines = vec![
        kv("Tab / Shift-Tab", "next / previous tab"),
        kv("1 – 5", "jump to tab"),
        kv("↑ ↓  /  j k", "move selection"),
        kv("g / G", "first / last"),
        kv("/", "search (Library)"),
        kv("s", "pick a folder + scan (engine)"),
        kv("r", "reload from DB"),
        kv("?", "toggle this help"),
        kv("q / Esc", "quit"),
    ];
    let w = 52.min(area.width.saturating_sub(4));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centered(area, w, h);
    overlay_bg(f, popup);
    let p = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(GOLD))
            .style(Style::default().bg(BG))
            .title(Span::styled(" Keys ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
    );
    f.render_widget(p, popup);
}

/// Single-line folder-path prompt (the `s` key): type a path, `Enter`/`Tab`
/// confirm, `Esc` cancel. Shows the typed text + a block cursor, `~` hint, and
/// an inline error (pink) when the last confirm hit a bad/!dir path.
fn render_input(f: &mut Frame, app: &App, area: Rect) {
    let w = 66.min(area.width.saturating_sub(4)).max(24);
    let h = 6;
    let popup = centered(area, w, h);
    overlay_bg(f, popup);

    let field_max = w.saturating_sub(5) as usize; // borders + "> " + cursor
    let mut lines = vec![
        Line::from(Span::styled("Folder to scan", Style::default().fg(CYAN).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{}\u{2588}", input_tail(&app.input, field_max)), Style::default().fg(FG)),
        ]),
    ];
    match &app.input_error {
        Some(err) => lines.push(Line::from(Span::styled(
            truncate(err, w.saturating_sub(2) as usize),
            Style::default().fg(PINK),
        ))),
        None => lines.push(Line::from(Span::styled("~ expands to your home folder", Style::default().fg(DIM)))),
    }
    lines.push(Line::from(Span::styled("Enter / Tab confirm · Esc cancel", Style::default().fg(DIM))));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(BG))
        .title(Span::styled(" Scan folder ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)));
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}

// ---- shared builders --------------------------------------------------------

/// Centered sub-rect, clamped to `area`.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// `Clear` resets cells to the terminal default; re-establish the brand-dark bg
/// over the popup so overlays stay legible on light terminals too (FIX 1).
fn overlay_bg(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
}

/// Show the END of a path field (what the user is typing) when it overflows,
/// prefixing `…`. Mirrors how shells keep the cursor end visible.
fn input_tail(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if max == 0 || count <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - (max - 1)).collect();
    format!("…{tail}")
}

fn render_list(f: &mut Frame, area: Rect, title: &str, items: Vec<ListItem>, cursor: usize, accent: Color) {
    let len = items.len();
    let list = List::new(items)
        .block(titled_block(title, accent))
        .highlight_style(Style::default().fg(Color::Black).bg(accent).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    if len > 0 {
        state.select(Some(cursor.min(len - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_empty(f: &mut Frame, area: Rect, title: &str, head: &str, sub: &str) {
    let text = Text::from(vec![
        Line::from(Span::styled(head, Style::default().fg(PINK).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(sub, Style::default().fg(DIM))),
    ]);
    f.render_widget(
        Paragraph::new(text).alignment(Alignment::Left).wrap(Wrap { trim: true }).block(titled_block(title, LAVENDER)),
        area,
    );
}

fn titled_block(title: &str, accent: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(format!(" {title} "), Style::default().fg(accent).add_modifier(Modifier::BOLD)))
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<16}"), Style::default().fg(DIM)),
        Span::styled(value.to_string(), Style::default().fg(FG)),
    ])
}

fn kind_span(kind: &str) -> Span<'static> {
    let color = match kind {
        "image" => CYAN,
        "video" => PINK,
        "audio" => LAVENDER,
        "doc" | "pdf" => GOLD,
        _ => DIM,
    };
    Span::styled(format!("{:<5}", truncate(kind, 5)), Style::default().fg(color))
}

fn conf_span(conf: &str) -> Span<'static> {
    let color = match conf {
        "auto" => CYAN,
        "review" => GOLD,
        _ => PINK,
    };
    Span::styled(format!("{conf:<6}"), Style::default().fg(color))
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Show the destination relative to its trailing segments (the category folder
/// + filename), which is what matters in the plan view.
fn rel_dest(dest: &str) -> String {
    let parts: Vec<&str> = dest.rsplit(['/', '\\']).take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Unix seconds → `YYYY-MM-DD HH:MM` UTC (Howard Hinnant civil-from-days; no
/// deps). Ported from the CLI's `info::unix_to_date`.
fn fmt_date(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".to_string();
    }
    let total = secs as i64;
    let days = total.div_euclid(86_400);
    let rem = total.rem_euclid(86_400);
    let (hh, mm) = (rem / 3600, (rem % 3600) / 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_chunks_splits_3_body_1() {
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let c = frame_chunks(area);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].height, 3);
        assert_eq!(c[2].height, 1);
        assert_eq!(c[1].height, 20);
        // chunks tile the area with no gap
        assert_eq!(c[0].height + c[1].height + c[2].height, 24);
    }

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(basename("/a/b/c.txt"), "c.txt");
        assert_eq!(basename("C:\\a\\b\\c.txt"), "c.txt");
        assert_eq!(basename("noslash"), "noslash");
    }

    #[test]
    fn rel_dest_keeps_two_tail_segments() {
        assert_eq!(rel_dest("/lib/Images/2021/p.jpg"), "2021/p.jpg");
    }

    #[test]
    fn fmt_date_epoch_and_known() {
        assert_eq!(fmt_date(0.0), "—");
        // 2021-01-01 00:00 UTC == 1609459200
        assert_eq!(fmt_date(1_609_459_200.0), "2021-01-01 00:00 UTC");
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("ab", 4), "ab");
    }

    /// Headless full-frame render via ratatui's TestBackend (no real terminal):
    /// proves the tab bar, Library pane, and live file data all paint.
    #[test]
    fn renders_library_frame_with_live_data() {
        use crate::app::App;
        use crate::data::{FileRow, LoadMsg, Snapshot};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new("/tmp/x.sqlite".into());
        let snap = Snapshot {
            db_exists: true,
            files: vec![FileRow {
                id: 1,
                path: "/tmp/corpus/report.txt".into(),
                kind: "doc".into(),
                extension: "txt".into(),
                size: 2048,
                modified: Some(1_609_459_200.0),
                has_text: true,
                has_faces: false,
            }],
            ..Snapshot::default()
        };
        app.apply_load(LoadMsg::Done(Box::new(snap)));

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(text.contains("FileID"), "brand/tab bar missing");
        assert!(text.contains("Library"), "Library pane title missing");
        assert!(text.contains("report.txt"), "live file row not rendered");
    }

    #[test]
    fn input_tail_keeps_end_visible() {
        assert_eq!(input_tail("/short", 20), "/short");
        // Overflow keeps the trailing chars (where the cursor is), prefixed `…`.
        let out = input_tail("/very/long/path/to/some/deep/folder", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.starts_with('…'));
        assert!(out.ends_with("folder"));
    }

    /// FIX 1 — the UI must paint its OWN dark background so it's legible on a
    /// light-background terminal. Every cell of the body region must carry the
    /// brand near-black `BG` (never the terminal default `Color::Reset`), and a
    /// known label (the gold brand accent) must render in a readable foreground.
    ///
    /// We sweep the body chunk specifically: it has no wide glyphs in the
    /// default render, so it's free of the continuation-cell `reset()` that
    /// ratatui applies after a 2-wide char (the `⏳` status spinner) — a cell
    /// the glyph still visually covers.
    #[test]
    fn paints_dark_background_and_gold_accent() {
        use crate::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (w, h) = (80u16, 24u16);
        let app = App::new("/tmp/x.sqlite".into());
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();

        // Body chunk = rows [3, h-1): every cell painted with the brand dark bg,
        // never left on the terminal's default (which would vanish on light bg).
        let body = frame_chunks(Rect { x: 0, y: 0, width: w, height: h })[1];
        for y in body.top()..body.bottom() {
            for x in 0..w {
                let bg = buf[(x, y)].bg;
                assert_ne!(bg, Color::Reset, "cell ({x},{y}) left on terminal-default bg");
                assert_eq!(bg, BG, "cell ({x},{y}) not painted with brand dark bg");
            }
        }

        // The gold brand accent renders somewhere (the ' FileID ' title / tabs),
        // proving a known label paints in a high-contrast fg on the dark bg.
        let has_gold = (0..h).any(|y| (0..w).any(|x| buf[(x, y)].fg == GOLD));
        assert!(has_gold, "gold brand accent not rendered in any cell");
    }
}
