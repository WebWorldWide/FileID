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

use crate::app::{dir_label, App, BrowseRow, Browser, DirCounts, Tab};
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

/// Top-level vertical split: tab bar (3) · body (rest) · status line (1) · key
/// bar (1). The key bar (FIX 1) is the always-visible bottom row showing the
/// available actions. Pure + deterministic so it's unit-testable without a
/// terminal. Body stays at index 1 (callers depend on that).
pub fn frame_chunks(area: Rect) -> Rc<[Rect]> {
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area)
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
    render_key_bar(f, app, chunks[3]);
    if app.show_help {
        render_help(f, area);
    }
    if let Some(browser) = &app.browser {
        render_browser(f, browser, area);
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
    // FIX 1 — scratch is the default (no `--db`): the TUI opens EMPTY here and
    // stays clean until the user scans a folder of their own. An explicit `--db`
    // that doesn't exist yet gets the original "resolved path" wording.
    let text = if app.scratch {
        Text::from(vec![
            Line::from(Span::styled(
                "No files yet.",
                Style::default().fg(PINK).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press  s  to browse for a folder and scan it — its files show up here.",
                Style::default().fg(GOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "This is a private scratch library; it starts empty and only ever holds",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                "what you scan in the TUI. To open an existing library instead, relaunch",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled("with  --db <path>  (e.g. your desktop app's).", Style::default().fg(DIM))),
            Line::from(""),
            Line::from(Span::styled(format!("scratch: {}", app.db_label), Style::default().fg(DIM))),
        ])
    } else {
        Text::from(vec![
            Line::from(Span::styled(
                "No library database found.",
                Style::default().fg(PINK).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!("Resolved path: {}", app.db_label)),
            Line::from(""),
            Line::from(Span::styled(
                "Press  s  to browse for a folder and scan it with the engine,",
                Style::default().fg(GOLD),
            )),
            Line::from(Span::styled(
                "or index with the CLI:  fileid scan <path> --models",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled("…then reload here with  r", Style::default().fg(DIM))),
        ])
    };
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
    let mut lines = vec![
        Line::from(Span::styled("Library", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        kv("mode", if app.scratch { "scratch (opens empty)" } else { "explicit --db / env" }),
        kv("db path", &app.db_label),
        kv("exists", if app.data.db_exists { "yes" } else { "no" }),
        kv("files indexed", &app.data.total_files.to_string()),
        kv("tags", &app.data.total_tags.to_string()),
        kv("people", &app.data.people.len().to_string()),
        kv("duplicate groups", &app.data.dupes.len().to_string()),
    ];
    if app.scratch {
        lines.push(Line::from(Span::styled(
            "Private scratch library — starts empty, holds only what you scan here.",
            Style::default().fg(DIM),
        )));
        lines.push(Line::from(Span::styled(
            "Open a specific library with  --db <path>  (e.g. your desktop app's).",
            Style::default().fg(DIM),
        )));
    }
    lines.extend(vec![
        Line::from(""),
        Line::from(Span::styled("Engine", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        kv("read surface", "fileid_engine::db::open_read (in-process)"),
        kv("plan", "fileid_engine::pipeline::restructure::classify"),
        kv("scan", "engine-spawn startScan IPC + live events (press s)"),
        Line::from(""),
        Line::from(Span::styled("Stubbed (follow-on)", Style::default().fg(PINK).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("· face clustering via engine-spawn IPC", Style::default().fg(DIM))),
        Line::from(Span::styled("· semantic search, restructure apply, people merge", Style::default().fg(DIM))),
    ]);
    let p = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }).block(titled_block("Settings", LAVENDER));
    f.render_widget(p, area);
}

/// The status line: a ⏳/✓ prefix + the live status message. The actionable
/// keys live on their OWN always-visible row (`render_key_bar`, FIX 1) so this
/// line can use the full width for the message.
fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let prefix = if app.loading { "⏳ " } else { "✓ " };
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(if app.loading { PINK } else { CYAN })),
        Span::styled(
            truncate(&app.status, area.width.saturating_sub(4) as usize),
            Style::default().fg(FG),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// FIX 1 — the always-visible key bar (the bottom row). Gold-accented `[key]`s
/// with dim labels so the user can SEE what to press without being told. The
/// hints are context-aware: a modal overlay (browser / search / typed-path /
/// help) surfaces its OWN keys; otherwise the active tab's actions are shown.
fn render_key_bar(f: &mut Frame, app: &App, area: Rect) {
    let hints = key_hints(app);
    let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 3);
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), Style::default().fg(FG)));
    }
    // Re-assert BG so the row stays legible on light terminals (FIX 1).
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(BG)), area);
}

/// Context-aware key hints for the bottom bar (FIX 1). Returned as
/// `(key, label)` pairs so the renderer can gold-accent the keys uniformly.
fn key_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.show_help {
        return vec![("?", "close help"), ("q", "quit")];
    }
    if app.input_active {
        return vec![("a-z", "type a path"), ("Enter", "confirm"), ("Esc", "cancel")];
    }
    if app.browser.is_some() {
        return vec![
            ("↑↓", "move"),
            ("Enter", "open"),
            ("Bksp", "up"),
            ("s", "scan this folder"),
            ("Esc", "cancel"),
        ];
    }
    if app.search_active {
        return vec![("a-z", "filter"), ("Enter", "done"), ("Esc", "clear")];
    }
    // Concise labels so the busiest tab (Library, 7 hints) still fits an 80-col
    // terminal without clipping `quit`; the `?` overlay carries fuller wording.
    let mut v = vec![("s", "scan")];
    if app.tab == Tab::Library {
        v.push(("/", "search"));
    }
    if app.tab != Tab::Settings {
        v.push(("↑↓", "move"));
    }
    v.push(("Tab", "switch"));
    v.push(("r", "reload"));
    v.push(("?", "help"));
    v.push(("q", "quit"));
    v
}

/// FIX 3 — the `?` help overlay: every key, the folder-browser keys, and the
/// macOS model-free-scan note. Kept compact enough to fit a 24-row terminal so
/// the macOS note is never clipped.
fn render_help(f: &mut Frame, area: Rect) {
    let header = |s: &'static str| {
        Line::from(Span::styled(s, Style::default().fg(GOLD).add_modifier(Modifier::BOLD)))
    };
    let note = |s: &'static str| Line::from(Span::styled(s, Style::default().fg(LAVENDER)));
    let lines = vec![
        kv("Tab / Shift-Tab", "next / previous tab"),
        kv("1 – 5", "jump to a tab"),
        kv("↑↓  /  j k", "move selection"),
        kv("g / G", "first / last in list"),
        kv("s", "browse folders + scan"),
        kv("/", "search (Library tab)"),
        kv("r", "reload from the library DB"),
        kv("?", "toggle this help"),
        kv("q / Esc", "quit"),
        Line::from(""),
        header("Folder browser (press s):"),
        kv("↑↓  Enter", "highlight / open a subfolder"),
        kv("Backspace / h", "go up a level"),
        kv("s", "scan THIS folder"),
        kv("t", "type a path instead · Esc cancel"),
        Line::from(""),
        note("macOS: full-AI scan needs the desktop app's models."),
        note("Without them: fileid scan <folder> (model-free), then r."),
    ];
    let w = 64.min(area.width.saturating_sub(4)).max(34);
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centered(area, w, h);
    overlay_bg(f, popup);
    let p = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(GOLD))
            .style(Style::default().bg(BG))
            .title(Span::styled(" Keys ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
    );
    f.render_widget(p, popup);
}

/// FIX 2 — the folder browser overlay (the `s` key). Shows the current folder as
/// a title, a scrollable list of its subdirectories (with a leading `..`), an
/// optional permission notice, and a one-line key hint. Pure read of `&Browser`.
fn render_browser(f: &mut Frame, browser: &Browser, area: Rect) {
    let w = 78.min(area.width.saturating_sub(2)).max(30);
    let h = 24.min(area.height.saturating_sub(2)).max(10);
    let popup = centered(area, w, h);
    overlay_bg(f, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(BG))
        .title(Span::styled(
            " Pick a folder to scan ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Layout: a fixed gold "scan this folder" button, the title (path + the
    // folder's OWN counts), the subdir list (with per-row counts), an optional
    // dimmed file preview, an optional notice, and the key hint.
    let files_h: u16 = if browser.files.is_empty() {
        0
    } else {
        (browser.files.len() as u16 + 1).min(7)
    };
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(2), Constraint::Min(3)];
    if files_h > 0 {
        constraints.push(Constraint::Length(files_h));
    }
    if browser.notice.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    let rows = Layout::vertical(constraints).split(inner);

    // (0) The headline affordance — a black-on-gold button so "scan THIS folder"
    // is unmissable, plus the `s` shortcut.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " [ → Scan this folder ] ",
                Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  press ", Style::default().fg(DIM)),
            Span::styled("s", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        ])),
        rows[0],
    );

    // (1) Title: the current folder (home-collapsed, tail-truncated) + its own
    // shallow counts, so the user sees what a scan here would pick up.
    let here = truncate(&short(&browser.cwd.to_string_lossy()), inner.width as usize);
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(here, Style::default().fg(CYAN).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(count_summary(&browser.here), Style::default().fg(DIM))),
        ])),
        rows[1],
    );

    // (2) The subdirectory list, WINDOWED to the visible rows so counts are
    // computed lazily (one shallow read_dir per on-screen row, cached).
    let list_area = rows[2];
    let total = browser.rows.len();
    let vh = (list_area.height as usize).max(1);
    let offset = if total <= vh { 0 } else { browser.selected.saturating_sub(vh / 2).min(total - vh) };
    let end = (offset + vh).min(total);
    let items: Vec<ListItem> = browser.rows[offset..end]
        .iter()
        .map(|row| match row {
            BrowseRow::Parent => {
                ListItem::new(Line::from(Span::styled("..   (up a level)", Style::default().fg(LAVENDER))))
            }
            BrowseRow::Dir(p) => {
                let counts = browser.count_for(p).map(|c| count_summary(&c));
                dir_row(&format!("{}/", dir_label(p)), counts.as_deref(), list_area.width)
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(browser.selected.saturating_sub(offset)));
    }
    f.render_stateful_widget(list, list_area, &mut state);

    let mut idx = 3;

    // (3) Dimmed preview of the files a scan here would touch (images flagged).
    if files_h > 0 {
        render_file_preview(f, browser, rows[idx]);
        idx += 1;
    }

    // (4) Permission/notice line.
    if let Some(notice) = &browser.notice {
        f.render_widget(
            Paragraph::new(Span::styled(truncate(notice, inner.width as usize), Style::default().fg(PINK))),
            rows[idx],
        );
        idx += 1;
    }

    // (5) Key hint.
    f.render_widget(
        Paragraph::new(Span::styled(
            "↑↓ move · Enter open · Bksp up · s scan this folder · t type · Esc cancel",
            Style::default().fg(DIM),
        )),
        rows[idx],
    );
}

/// The dimmed "files here" preview inside the browser (FIX 2): a header with the
/// total, then the file names (image files cyan-flagged), collapsing the
/// overflow into a `+N more` line.
fn render_file_preview(f: &mut Frame, browser: &Browser, area: Rect) {
    let width = area.width as usize;
    let header = format!(
        "Files here ({}{}):",
        browser.files_total,
        if browser.here.capped { "+" } else { "" }
    );
    let mut lines = vec![Line::from(Span::styled(header, Style::default().fg(DIM).add_modifier(Modifier::BOLD)))];

    let body_cap = (area.height as usize).saturating_sub(1);
    let kept = browser.files.len();
    let need_more = kept > body_cap || browser.files_total > kept || browser.here.capped;
    let show = if need_more { body_cap.saturating_sub(1) } else { body_cap };

    for fe in browser.files.iter().take(show) {
        let (marker, color) = if fe.is_image { ("▪ ", CYAN) } else { ("· ", DIM) };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(color)),
            Span::styled(truncate(&fe.name, width.saturating_sub(2)), Style::default().fg(DIM)),
        ]));
    }
    if need_more {
        let remaining = browser.files_total.saturating_sub(show);
        let plus = if browser.here.capped { "+" } else { "" };
        lines.push(Line::from(Span::styled(
            format!("  … and {remaining}{plus} more"),
            Style::default().fg(DIM),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// One subdirectory row: `Name/` on the left (FG), its shallow counts dim and
/// right-aligned. `counts == None` means the folder was unreadable.
fn dir_row(name: &str, counts: Option<&str>, width: u16) -> ListItem<'static> {
    let usable = (width as usize).saturating_sub(2); // the "▶ " highlight gutter
    let counts = counts.unwrap_or("· unreadable");
    let cw = counts.chars().count();
    let name_room = usable.saturating_sub(cw + 1).max(4);
    let name_t = truncate(name, name_room);
    let used = name_t.chars().count() + cw;
    let pad = usable.saturating_sub(used).max(1);
    ListItem::new(Line::from(vec![
        Span::styled(name_t, Style::default().fg(FG)),
        Span::raw(" ".repeat(pad)),
        Span::styled(counts.to_string(), Style::default().fg(DIM)),
    ]))
}

/// Compact one-line tally for a folder: `143 images · 27 files · 12 folders`
/// (FIX 2). A `+` suffix marks a count that hit the shallow walk cap.
fn count_summary(c: &DirCounts) -> String {
    format!(
        "{} · {} · {}",
        count_part(c.images, c.capped, "image", "images"),
        count_part(c.files, c.capped, "file", "files"),
        count_part(c.dirs, c.capped, "folder", "folders"),
    )
}

fn count_part(n: usize, capped: bool, singular: &str, plural: &str) -> String {
    let word = if n == 1 && !capped { singular } else { plural };
    let suffix = if capped { "+" } else { "" };
    format!("{n}{suffix} {word}")
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
    fn frame_chunks_splits_tabs_body_status_keybar() {
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let c = frame_chunks(area);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].height, 3); // tab bar
        assert_eq!(c[2].height, 1); // status line
        assert_eq!(c[3].height, 1); // always-visible key bar (FIX 1)
        assert_eq!(c[1].height, 19); // body gets the rest
        // chunks tile the area with no gap
        assert_eq!(c[0].height + c[1].height + c[2].height + c[3].height, 24);
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

    /// Sweep a whole `TestBackend` buffer into one string (row-major).
    fn frame_text(w: u16, h: u16, app: &crate::app::App) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// FIX 1 — the always-visible key bar shows the active tab's actions. On
    /// Library it advertises scan + search; switching to People drops `search`.
    #[test]
    fn key_bar_advertises_tab_actions_and_fits_80_cols() {
        use crate::app::{App, Tab};

        let mut app = App::new("/tmp/x.sqlite".into());
        let lib = frame_text(80, 24, &app);
        // The bottom row (the key bar) must carry the headline keys.
        let bar = lib.lines().last().unwrap_or("");
        assert!(bar.contains("[s]") && bar.contains("scan"), "key bar missing scan: {bar:?}");
        assert!(bar.contains("[/]") && bar.contains("search"), "Library bar missing search: {bar:?}");
        assert!(bar.contains("[q]") && bar.contains("quit"), "key bar missing quit: {bar:?}");
        // Everything fits an 80-col row: no trailing glyph is lost off the edge.
        assert!(bar.trim_end().chars().count() <= 80, "key bar overflows 80 cols: {bar:?}");

        // People tab: search is Library-only, so it drops from the bar.
        app.tab = Tab::People;
        let ppl = frame_text(80, 24, &app);
        let bar = ppl.lines().last().unwrap_or("");
        assert!(bar.contains("[s]"), "key bar missing scan off-Library: {bar:?}");
        assert!(!bar.contains("search"), "search hint must be Library-only: {bar:?}");
    }

    /// FIX 2 — the folder browser overlay paints the current folder, its
    /// subdirectories (with a `..` row), and the one-line key hint.
    #[test]
    fn browser_overlay_renders_cwd_subdirs_counts_files_and_affordance() {
        use crate::app::{App, Browser};

        let base = std::env::temp_dir().join(format!("fileid-ui-browse-{}", std::process::id()));
        std::fs::create_dir_all(base.join("Pictures")).unwrap();
        std::fs::create_dir_all(base.join("Documents")).unwrap();
        std::fs::write(base.join("photo.png"), "img").unwrap();
        std::fs::write(base.join("notes.txt"), "txt").unwrap();

        let mut app = App::new("/tmp/x.sqlite".into());
        app.browser = Some(Browser::open(base.clone()));
        let text = frame_text(100, 30, &app);

        assert!(text.contains("Pick a folder to scan"), "browser title missing");
        // FIX 2 — the unmissable scan affordance + subdir rows + counts.
        assert!(text.contains("Scan this folder"), "scan affordance missing");
        assert!(text.contains("Pictures/"), "subdir Pictures not listed");
        assert!(text.contains("Documents/"), "subdir Documents not listed");
        assert!(text.contains("folders"), "per-folder/own counts missing");
        // FIX 2 — the dimmed preview lists the actual files a scan would touch.
        assert!(text.contains("Files here"), "file preview header missing");
        assert!(text.contains("photo.png"), "preview file not listed");
        assert!(text.contains(".."), "the up-a-level row is missing");
        assert!(text.contains("scan this folder"), "browser key hint missing");

        let _ = std::fs::remove_dir_all(&base);
    }
}
