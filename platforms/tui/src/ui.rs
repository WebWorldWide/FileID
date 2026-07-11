//! Rendering. Pure read of `&App` → ratatui widgets. No state mutation here.
//!
//! The look mirrors the FileID terminal design: calm near-black panels, the
//! signature accents (gold `#FFCC00`, lavender `#B19BCE`, cyan `#A0E2EA`, pink
//! `#F2A6C0`) used sparingly, plus a quiet green `#86d9a4` for "kept / safe /
//! on-device". Brand `FileID` + numbered tabs sit on top with a gold underline
//! under the active tab; every screen states what it is in plain words; the keys
//! you can press are always pinned along the bottom in pill-styled hints.
//!
//! READABLE ON ANY TERMINAL THEME: we paint our OWN brand-dark background (`BG`)
//! across the whole frame first, with a near-white default foreground (`FG`);
//! every widget below patches only its `fg`, inheriting the dark `bg`. So a
//! light-background terminal can't wash out the gold/light accents — we never
//! depend on the terminal's default colors anywhere visible (overlays
//! re-establish `BG` after `Clear`). The selected row is a gold-tinted band with
//! a solid gold left edge — the terminal stand-in for the mockup's `inset 3px 0
//! #FFCC00` highlight.

use std::rc::Rc;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{dir_label, App, BrowseRow, Browser, DirCounts, DownloadState, Tab};
use crate::data::{human_size, short};

// ── Palette — lifted from the design's terminal panels (NOT the beige page) ──
/// Main panel background (`#141417`). Painted under the whole UI so light
/// terminal themes can't render the gold/light foreground invisibly.
const BG: Color = Color::Rgb(20, 20, 23);
/// Elevated surface (`#1b1b21`) — modals, overlays.
const SURFACE: Color = Color::Rgb(27, 27, 33);
/// Near-white body text (`#e6e6ea`) — strong contrast on `BG`.
const FG: Color = Color::Rgb(230, 230, 234);
/// Quieter body text for unselected list rows (`#cfcfd6`).
const SECONDARY: Color = Color::Rgb(207, 207, 214);
/// Secondary labels (`#8a8a94`).
const DIM: Color = Color::Rgb(138, 138, 148);
/// Tertiary / hint text (`#74747e`).
const FAINT: Color = Color::Rgb(116, 116, 126);
/// Inactive tab label (`#76767f`).
const TAB_OFF: Color = Color::Rgb(118, 118, 127);
/// Hairline divider (`#25252c`).
const DIVIDER: Color = Color::Rgb(37, 37, 44);
/// Panel border (`#2a2a32`).
const BORDER: Color = Color::Rgb(42, 42, 50);
/// Key-pill background (`#23232b`).
const PILL_BG: Color = Color::Rgb(35, 35, 43);
const GOLD: Color = Color::Rgb(255, 204, 0);
/// Muted gold for a focused panel's border — `rgba(255,204,0,.30)` over `BG`.
const GOLD_DIM: Color = Color::Rgb(91, 76, 21);
/// Selected-row tint — `rgba(255,204,0,.14)` over `BG`.
const SEL_BG: Color = Color::Rgb(53, 46, 20);
const LAVENDER: Color = Color::Rgb(177, 155, 206);
const CYAN: Color = Color::Rgb(160, 226, 234);
const PINK: Color = Color::Rgb(242, 166, 192);
/// "Kept / safe / on-device" green (`#86d9a4`) — privacy + keep markers.
const GREEN: Color = Color::Rgb(134, 217, 164);
/// The install gauge's unfilled track — a dark gold-brown (`#302a1c`) so the
/// gold fill reads clearly against it inside the gold-tinted banner band.
const GAUGE_TRACK: Color = Color::Rgb(48, 42, 28);

/// Top-level vertical split: header (2 — brand+tabs row, then a divider with the
/// active-tab underline) · body (rest) · status line (1) · key bar (1). The key
/// bar is the always-visible bottom row of actions. Pure + deterministic so it's
/// unit-testable without a terminal. Body stays at index 1 (callers depend on
/// that).
pub fn frame_chunks(area: Rect) -> Rc<[Rect]> {
    Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area)
}

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    // Paint the brand-dark background under everything FIRST. A Block's style
    // fills the whole area's bg; subsequent widgets patch only fg, so the dark
    // bg shows through — legible on light AND dark terminals alike.
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);

    let chunks = frame_chunks(area);
    render_header(f, app, chunks[0]);
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

/// The header: `FileID` brand (gold) + numbered tabs on row 0, then a full-width
/// hairline on row 1 with a bright-gold underline drawn under the active tab —
/// the terminal stand-in for the mockup's `border-bottom:2px solid #FFCC00`.
fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::with_capacity(Tab::ALL.len() * 2 + 2);
    spans.push(Span::styled(
        "FileID",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("   "));
    let mut col = area.x.saturating_add(9); // "FileID" (6) + 3-space gap
    let mut active: Option<(u16, u16)> = None;
    for (i, t) in Tab::ALL.iter().enumerate() {
        let label = format!("{} {}", i + 1, t.title());
        let w = label.chars().count() as u16;
        if *t == app.tab {
            spans.push(Span::styled(
                label,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            active = Some((col, w));
        } else {
            spans.push(Span::styled(label, Style::default().fg(TAB_OFF)));
        }
        col = col.saturating_add(w);
        if i + 1 < Tab::ALL.len() {
            spans.push(Span::raw("   "));
            col = col.saturating_add(3);
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );

    if area.height < 2 {
        return;
    }
    let row1 = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let rule = "─".repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Span::styled(rule, Style::default().fg(DIVIDER))),
        row1,
    );
    if let Some((sx, w)) = active {
        let right = area.x.saturating_add(area.width);
        if w > 0 && sx < right {
            let seg_w = w.min(right - sx);
            let seg = Rect {
                x: sx,
                y: area.y + 1,
                width: seg_w,
                height: 1,
            };
            let bar = "─".repeat(seg_w as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    bar,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                )),
                seg,
            );
        }
    }
}

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    // A standing "models missing" / "installing" banner sits ABOVE every tab's
    // content so a fresh install can't miss that the AI models need downloading.
    let area = render_model_banner(f, app, area);
    // The first-run welcome stands in for the EMPTY Library only. Every other tab
    // (Settings included) must still render even before the first scan creates the
    // library DB — otherwise the welcome screen masks the whole UI and Settings /
    // the model-download prompt become unreachable.
    if !app.data.db_exists && !app.loading && app.tab == Tab::Library {
        render_welcome(f, app, area);
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

/// A standing, hard-to-miss banner pinned above every tab's body whenever the AI
/// models aren't installed (or are downloading). Returns the body area to render
/// below it: the FULL area when there's nothing to announce, otherwise the area
/// minus its one banner row. Painted as a gold-tinted band (the same tint as a
/// selected row) so it reads as a persistent notice — NOT a fleeting status-line
/// message — until the models are present or a download finishes.
fn render_model_banner(f: &mut Frame, app: &App, area: Rect) -> Rect {
    // A live install takes the banner slot, replacing the static notice with a
    // real progress gauge that visibly fills 0→100.
    if let Some(dl) = &app.download {
        if area.height >= 2 {
            return render_download_gauge(f, dl, area);
        }
        return area;
    }
    if app.missing_models.is_empty() || area.height < 2 {
        return area;
    }
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let pill = Style::default()
        .fg(Color::Black)
        .bg(GOLD)
        .add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        Span::styled(" ⚠ ", pill),
        Span::styled(
            "  AI models not installed — press ",
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" D ", pill),
        Span::styled(
            " to download (~1.6 GB). Tags, faces & search need them.",
            Style::default().fg(FG),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(SEL_BG)),
        rows[0],
    );
    rows[1]
}

/// The AI-model install gauge (the SHARED CONTRACT made visible). A gold-tinted
/// band pinned above every tab while a `fileid models download` runs: a title
/// row (`⟳ Installing AI models…`, or a green `✓ … press s to scan` once done),
/// a ratatui [`Gauge`] (gold fill on a dark track, the overall percent centered),
/// and — where there's room — the live label beneath (`arcface · 182/271 MB · …`).
/// Returns the body area below the band. Degrades to a 2-row band (no label) on
/// short terminals; the percent is clamped so the widget can't panic.
fn render_download_gauge(f: &mut Frame, dl: &DownloadState, area: Rect) -> Rect {
    let pct = dl.percent.min(100);
    let band_h = if area.height >= 5 { 3 } else { 2 };
    let rows = Layout::vertical([Constraint::Length(band_h), Constraint::Min(0)]).split(area);
    let band = rows[0];
    // Paint the whole band the standing-notice gold tint first; the gauge row
    // overwrites its slice with the track colour.
    f.render_widget(Block::default().style(Style::default().bg(SEL_BG)), band);

    let inner = if band_h == 3 {
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(band)
    } else {
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(band)
    };

    let title = if dl.done {
        Line::from(vec![
            Span::styled(
                " ✓ ",
                Style::default()
                    .fg(Color::Black)
                    .bg(GREEN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  AI models installed — press ",
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " s ",
                Style::default()
                    .fg(Color::Black)
                    .bg(GOLD)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to scan with full AI", Style::default().fg(GREEN)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " ⟳ ",
                Style::default()
                    .fg(Color::Black)
                    .bg(GOLD)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Installing AI models…",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
        ])
    };
    f.render_widget(
        Paragraph::new(title).style(Style::default().bg(SEL_BG)),
        inner[0],
    );

    let bar = if dl.done { GREEN } else { GOLD };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(bar).bg(GAUGE_TRACK))
        .percent(pct)
        .label(Span::styled(
            format!("{pct}%"),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(gauge, inner[1]);

    if band_h == 3 {
        let label = truncate(&dl.label, band.width.saturating_sub(2) as usize);
        f.render_widget(
            Paragraph::new(Span::styled(format!("  {label}"), Style::default().fg(DIM)))
                .style(Style::default().bg(SEL_BG)),
            inner[2],
        );
    }
    rows[1]
}

/// First-run / empty-library welcome. Plain words about what FileID is, three
/// numbered next-steps in brand colors, and the on-device privacy reassurance.
fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let step = |n: &'static str, color: Color, head: &'static str, sub: &'static str| {
        vec![
            Line::from(vec![
                Span::styled(
                    format!("  {n}  "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(head, Style::default().fg(FG)),
            ]),
            Line::from(Span::styled(
                format!("     {sub}"),
                Style::default().fg(FAINT),
            )),
        ]
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "Welcome to FileID",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Point FileID at a folder and it builds one searchable library that",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::styled("understands what's ", Style::default().fg(DIM)),
            Span::styled("inside", Style::default().fg(SECONDARY)),
            Span::styled(
                " your files — photos, PDFs, videos, docs.",
                Style::default().fg(DIM),
            ),
        ]),
        Line::from(""),
    ];
    lines.extend(step(
        "1",
        GOLD,
        "Press  s  to pick a folder to scan",
        "a file browser opens — no typing needed",
    ));
    lines.extend(step(
        "2",
        CYAN,
        "Wait while it reads your files",
        "progress shows on the status line below",
    ));
    lines.extend(step(
        "3",
        LAVENDER,
        "Browse, search, and tidy up",
        "switch tabs with Tab or the number keys",
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("● ", Style::default().fg(GREEN)),
        Span::styled(
            "Everything stays on this computer. No cloud, no telemetry.",
            Style::default().fg(DIM),
        ),
    ]));

    if app.scratch {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "This is a private scratch library — it starts empty and only ever holds",
            Style::default().fg(FAINT),
        )));
        lines.push(Line::from(Span::styled(
            "what you scan here. Open another with  --db <path>  (e.g. the desktop app's).",
            Style::default().fg(FAINT),
        )));
        lines.push(Line::from(Span::styled(
            format!("scratch: {}", app.db_label),
            Style::default().fg(FAINT),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Resolved path: {}", app.db_label),
            Style::default().fg(FAINT),
        )));
        lines.push(Line::from(Span::styled(
            "Or index from the CLI:  fileid scan <path> --models  …then reload with  r",
            Style::default().fg(FAINT),
        )));
    }

    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: true })
        .block(focus_block("FileID"));
    f.render_widget(p, area);
}

fn render_library(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let visible = app.visible_files();

    // Context line: a live scan note, the active search, or the plain-language
    // search prompt — with the file count pinned to the right.
    if app.scanning {
        let status = truncate(&app.status, rows[0].width.saturating_sub(22) as usize);
        render_context(
            f,
            rows[0],
            vec![
                Span::styled("⟳ ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled("Reading your files…  ", Style::default().fg(GOLD)),
                Span::styled(status, Style::default().fg(DIM)),
            ],
            None,
        );
    } else {
        let left = if app.search_active {
            vec![
                Span::styled("⌕ ", Style::default().fg(CYAN)),
                Span::styled(app.search.clone(), Style::default().fg(FG)),
                Span::styled("█", Style::default().fg(GOLD)),
            ]
        } else if !app.search.is_empty() {
            let matches = if app.data.files_truncated {
                format!("{}+ matches", visible.len())
            } else {
                plural(visible.len(), "match", "matches")
            };
            vec![
                Span::styled("⌕ ", Style::default().fg(CYAN)),
                Span::styled(format!("/{}", app.search), Style::default().fg(FG)),
                Span::styled(format!("   {matches}"), Style::default().fg(DIM)),
            ]
        } else {
            vec![
                Span::styled("⌕ ", Style::default().fg(CYAN)),
                Span::styled(
                    "Type to search by name or what's inside…",
                    Style::default().fg(FAINT),
                ),
            ]
        };
        let count = if app.search.is_empty() {
            format!("{} files", app.data.total_files)
        } else if app.data.files_truncated {
            format!("{}+ matches", visible.len())
        } else {
            plural(visible.len(), "match", "matches")
        };
        render_context(f, rows[0], left, Some(count));
    }

    // Empty / no-match state: a full-width panel that says what to do, instead of
    // a blank two-column list the user can't act on. While a scan is running we
    // keep the (filling) list so the "Reading your files…" context reads right.
    if visible.is_empty() && !app.scanning {
        if !app.search.is_empty() {
            render_empty(
                f,
                rows[1],
                "Files",
                "No matches.",
                &format!(
                    "Nothing in this library matches \u{201c}{}\u{201d}.",
                    app.search
                ),
                Some(cta("Esc", "clear the search")),
            );
        } else {
            render_empty(
                f,
                rows[1],
                "Files",
                "No files yet.",
                "FileID builds one searchable library from a folder you pick — photos, PDFs, videos and docs, with tags and text it reads on-device.",
                Some(cta("s", "pick a folder and scan it")),
            );
        }
        return;
    }

    let cursor = app.cursor_clamped(visible.len());
    let cols =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[1]);
    let cw = content_width(cols[0]);
    render_calm_list(f, cols[0], "Files", &visible, cursor, |fr| file_row(cw, fr));
    render_file_detail(f, app, cols[1], visible.get(cursor).copied());
}

fn render_file_detail(f: &mut Frame, app: &App, area: Rect, file: Option<&crate::data::FileRow>) {
    let block = titled_block("Preview", CYAN);
    let Some(fr) = file else {
        let p = Paragraph::new(Span::styled(
            "Select a file to see its details.",
            Style::default().fg(DIM),
        ))
        .block(block);
        f.render_widget(p, area);
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "▦  no inline preview in the terminal",
            Style::default().fg(FAINT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            basename(&fr.path),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(short(&fr.path), Style::default().fg(FAINT))),
        Line::from(""),
        kv(
            "Kind",
            &format!(
                "{} · {}",
                friendly_kind(&fr.kind),
                fr.extension.to_uppercase()
            ),
        ),
        kv("Size", &human_size(fr.size)),
        kv(
            "Modified",
            &fr.modified.map_or_else(|| "—".into(), fmt_date),
        ),
    ];
    let contains = match (fr.has_text, fr.has_faces) {
        (true, true) => Some("text · faces"),
        (true, false) => Some("text"),
        (false, true) => Some("faces"),
        (false, false) => None,
    };
    if let Some(c) = contains {
        lines.push(kv("Contains", c));
    }

    lines.push(Line::from(""));
    lines.push(section("Tags"));
    match app.data.tags.get(&fr.id).filter(|t| !t.is_empty()) {
        Some(tags) => lines.push(Line::from(Span::styled(
            tags.join("  ·  "),
            Style::default().fg(LAVENDER),
        ))),
        None => lines.push(Line::from(Span::styled(
            "none yet",
            Style::default().fg(FAINT),
        ))),
    }

    if let Some(snip) = app.data.snippets.get(&fr.id) {
        lines.push(Line::from(""));
        lines.push(section("What's in it"));
        lines.push(Line::from(Span::styled(
            snip.clone(),
            Style::default().fg(DIM),
        )));
    }

    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: true })
        .block(block);
    f.render_widget(p, area);
}

fn render_people(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    if app.data.people.is_empty() {
        render_context(
            f,
            rows[0],
            vec![Span::styled(
                "People — faces the engine grouped automatically.",
                Style::default().fg(DIM),
            )],
            None,
        );
        render_empty(
            f,
            rows[1],
            "Groups",
            "No people yet.",
            "FileID detects faces during a full scan and groups the ones that match into people you can name. Needs the AI face models installed.",
            Some(cta("s", "scan a folder to detect & group faces")),
        );
        return;
    }
    render_context(
        f,
        rows[0],
        vec![Span::styled(
            format!(
                "FileID grouped the faces it found into {}.",
                plural(app.data.people.len(), "group", "groups")
            ),
            Style::default().fg(DIM),
        )],
        None,
    );
    let cw = content_width(rows[1]);
    render_calm_list(f, rows[1], "Groups", &app.data.people, app.cursor(), |p| {
        person_row(cw, p)
    });
}

fn render_cleanup(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    if app.data.dupes.is_empty() {
        render_context(
            f,
            rows[0],
            vec![Span::styled(
                "Cleanup — files saved more than once.",
                Style::default().fg(DIM),
            )],
            None,
        );
        render_empty(
            f,
            rows[1],
            "Duplicate sets",
            "No duplicates found.",
            "Cleanup spots files saved more than once by hashing their contents during a scan. It's a read-only preview — FileID never deletes anything here.",
            Some(cta("s", "scan a folder to check for duplicates")),
        );
        return;
    }
    render_context(
        f,
        rows[0],
        vec![
            Span::styled("Same file saved more than once. ", Style::default().fg(DIM)),
            Span::styled("This is a read-only preview", Style::default().fg(GREEN)),
            Span::styled(
                " — the first copy is the one worth keeping.",
                Style::default().fg(DIM),
            ),
        ],
        None,
    );

    let cols =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[1]);
    let cursor = app.cursor();
    let cw = content_width(cols[0]);
    render_calm_list(f, cols[0], "Duplicate sets", &app.data.dupes, cursor, |g| {
        dup_row(cw, g)
    });

    // Detail: the copies in the selected set — the first marked KEEP (green), the
    // rest flagged as duplicates. Read-only: FileID never deletes here.
    let block = titled_block("Copies", CYAN);
    let detail = match app.data.dupes.get(cursor) {
        Some(g) => {
            let mut lines = vec![
                section(&format!(
                    "{} of this file",
                    plural(g.copies as usize, "copy", "copies")
                )),
                Line::from(""),
            ];
            for (i, p) in g.paths.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "keep ",
                            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(short(p), Style::default().fg(SECONDARY)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("dup  ", Style::default().fg(PINK)),
                        Span::styled(short(p), Style::default().fg(DIM)),
                    ]));
                }
            }
            if g.copies > g.paths.len() as i64 {
                lines.push(Line::from(Span::styled(
                    format!("… and {} more copies", g.copies - g.paths.len() as i64),
                    Style::default().fg(FAINT),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Read-only preview — FileID never deletes anything here.",
                Style::default().fg(FAINT),
            )));
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true })
        }
        None => Paragraph::new(Span::styled("—", Style::default().fg(DIM))),
    };
    f.render_widget(detail.block(block), cols[1]);
}

fn render_restructure(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    if app.data.plan.is_empty() {
        render_context(
            f,
            rows[0],
            vec![Span::styled(
                "Restructure — a suggested tidy-up.",
                Style::default().fg(DIM),
            )],
            None,
        );
        render_empty(
            f,
            rows[1],
            "Suggested moves",
            "No moves to suggest yet.",
            "Restructure previews a tidy, dated folder layout from your indexed files — a read-only plan you review before anything moves.",
            Some(cta("s", "scan a folder, then this plan fills in")),
        );
        return;
    }
    render_context(
        f,
        rows[0],
        vec![
            Span::styled(
                "Suggested tidy-up — read-only preview, nothing moves.   ",
                Style::default().fg(DIM),
            ),
            Span::styled("● ", Style::default().fg(GREEN)),
            Span::styled("auto  ", Style::default().fg(DIM)),
            Span::styled("◐ ", Style::default().fg(GOLD)),
            Span::styled("check", Style::default().fg(DIM)),
        ],
        None,
    );
    let cw = content_width(rows[1]);
    render_calm_list(
        f,
        rows[1],
        "Suggested moves",
        &app.data.plan,
        app.cursor(),
        |m| plan_row(cw, m),
    );
}

/// The live AI-model status line for Settings: a green "all installed", an
/// in-flight "Installing… NN%" while a download runs, or a gold "not installed —
/// <names>" listing exactly which weights are missing. Mirrors the standing
/// banner so the same truth shows in both places.
fn model_status_line(app: &App) -> Line<'static> {
    if let Some(dl) = &app.download {
        return if dl.done {
            Line::from(vec![
                Span::styled("● ", Style::default().fg(GREEN)),
                Span::styled("All AI models installed.", Style::default().fg(FG)),
            ])
        } else {
            Line::from(vec![
                Span::styled("● ", Style::default().fg(GOLD)),
                Span::styled(
                    format!("Installing… {}%", dl.percent.min(100)),
                    Style::default().fg(FG),
                ),
            ])
        };
    }
    if app.missing_models.is_empty() {
        Line::from(vec![
            Span::styled("● ", Style::default().fg(GREEN)),
            Span::styled(
                "Status: all required models installed.",
                Style::default().fg(FG),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("● ", Style::default().fg(GOLD)),
            Span::styled(
                format!("Status: not installed — {}", app.missing_models.join(", ")),
                Style::default().fg(FG),
            ),
        ])
    }
}

fn render_settings(f: &mut Frame, app: &App, area: Rect) {
    let bullet = |text: &'static str| {
        Line::from(vec![
            Span::styled("● ", Style::default().fg(GREEN)),
            Span::styled(text, Style::default().fg(FG)),
        ])
    };
    let mut lines = vec![
        section("Library"),
        kv("Mode", if app.scratch { "scratch (opens empty)" } else { "explicit --db / env" }),
        kv("Saved at", &app.db_label),
        kv("Exists", if app.data.db_exists { "yes" } else { "no" }),
        kv("Files indexed", &app.data.total_files.to_string()),
        kv("Tags", &app.data.total_tags.to_string()),
        kv("People", &app.data.people.len().to_string()),
        kv("Duplicate sets", &app.data.dupes.len().to_string()),
        Line::from(""),
        section("Privacy"),
        bullet("Everything runs on this computer."),
        bullet("No cloud, no telemetry — ever."),
        bullet("Only network use: downloading AI models from huggingface.co."),
        Line::from(""),
        section("AI models"),
        model_status_line(app),
        Line::from(vec![
            Span::styled(
                " D ",
                Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Download all AI models for full scanning", Style::default().fg(FG)),
        ]),
        Line::from(Span::styled(
            "Needed for tags, faces & search. Fetched from huggingface.co; a progress bar shows above.",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        section("Folder browser"),
        Line::from(Span::styled(
            "Press s anywhere to browse:  ↑↓ move · Enter open · ← up · d drives · . hidden files.",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        section("Under the hood"),
        Line::from(Span::styled(
            "Reads the same library DB as the CLI and desktop apps (no contract drift).",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "Scan (s) drives the engine; face clustering & semantic search are follow-ons.",
            Style::default().fg(DIM),
        )),
    ];
    if app.scratch {
        lines.insert(
            8,
            Line::from(Span::styled(
                "Private scratch library — holds only what you scan here.",
                Style::default().fg(FAINT),
            )),
        );
    }
    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: true })
        .block(titled_block("Settings", CYAN));
    f.render_widget(p, area);
}

/// The status line: a ✓/⏳ prefix + the live status message. The actionable keys
/// live on their OWN always-visible row (`render_key_bar`).
fn render_status(f: &mut Frame, app: &App, area: Rect) {
    // A failure must not wear the success ✓: an errored scan/download/load shows
    // a persistent ⚠ until the next action, so the feedback can't be mistaken for
    // "done OK".
    let (icon, color) = if app.status_error {
        ("⚠", PINK)
    } else if app.loading {
        ("⏳", PINK)
    } else {
        ("✓", GREEN)
    };
    let line = Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(color)),
        Span::styled(
            truncate(&app.status, area.width.saturating_sub(3) as usize),
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line).style(Style::default().bg(BG)), area);
}

/// The always-visible key bar (bottom row). Each key is a dark pill with gold
/// text (the terminal stand-in for the mockup's `#23232b` key chips) + a dim
/// label. The hints are context-aware: a modal overlay surfaces its OWN keys;
/// otherwise the active tab's actions show, with `press ? for all keys` pinned
/// to the right.
fn render_key_bar(f: &mut Frame, app: &App, area: Rect) {
    // Re-assert BG so the row stays legible on light terminals.
    f.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let hints = key_hints(app);
    let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 3);
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(GOLD)
                .bg(PILL_BG)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), Style::default().fg(DIM)));
    }

    // The `press ? for all keys` tail is for the normal tab views only — modal
    // states already pin their own keys and need the full width.
    let modal = app.show_help || app.input_active || app.browser.is_some() || app.search_active;
    const TAIL_W: u16 = 22;
    if !modal && area.width > 40 + TAIL_W {
        let parts =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(TAIL_W)]).split(area);
        f.render_widget(Paragraph::new(Line::from(spans)), parts[0]);
        let tail = Line::from(vec![
            Span::styled("press ", Style::default().fg(FAINT)),
            Span::styled(
                " ? ",
                Style::default()
                    .fg(GOLD)
                    .bg(PILL_BG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for all keys", Style::default().fg(FAINT)),
        ]);
        f.render_widget(Paragraph::new(tail).alignment(Alignment::Right), parts[1]);
    } else {
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// Context-aware key hints for the bottom bar. Returned as `(key, label)` pairs
/// so the renderer can pill-style the keys uniformly. Only keys that actually do
/// something are advertised (no aspirational actions).
fn key_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.show_help {
        return vec![("?", "close help"), ("q", "quit")];
    }
    if app.input_active {
        return vec![
            ("a-z", "type a path"),
            ("Enter", "confirm"),
            ("Esc", "cancel"),
        ];
    }
    if let Some(b) = &app.browser {
        // `hidden off`/`hidden on` are both static, so the bar stays a Vec of
        // `&'static str` while still reflecting the toggle state (FEATURE 2).
        let hidden = if b.show_hidden {
            "hidden on"
        } else {
            "hidden off"
        };
        return vec![
            ("↑↓", "move"),
            ("Bksp", "up"),
            ("d", "drives"),
            (".", hidden),
            ("s", "scan"),
            ("Esc", "cancel"),
        ];
    }
    if app.search_active {
        return vec![("a-z", "filter"), ("Enter", "done"), ("Esc", "clear")];
    }
    // Concise so the busiest tab (Library) still fits an 80-col terminal with the
    // right-pinned `? for all keys` tail; the `?` overlay carries fuller wording.
    let mut v = Vec::with_capacity(6);
    if app.tab != Tab::Settings {
        v.push(("↑↓", "move"));
    }
    if app.tab == Tab::Library {
        v.push(("/", "search"));
    }
    v.push(("Tab", "switch"));
    v.push(("s", "scan"));
    if app.tab == Tab::Settings {
        v.push(("D", "get AI models"));
    }
    v.push(("q", "quit"));
    v
}

/// The `?` help overlay: every key, the folder-browser keys, and the model /
/// runtime download notes. Kept compact enough to fit a 24-row terminal.
fn render_help(f: &mut Frame, area: Rect) {
    let header = |s: &'static str| {
        Line::from(Span::styled(
            s,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
    };
    let note = |s: &'static str| Line::from(Span::styled(s, Style::default().fg(LAVENDER)));
    let lines = vec![
        hrow("Tab / Shift-Tab", "next / previous tab"),
        hrow("1 – 5", "jump to a tab"),
        hrow("↑↓  /  j k", "move selection"),
        hrow("g / G", "first / last in list"),
        hrow("s", "browse folders + scan"),
        hrow("D", "download AI models (any tab)"),
        hrow("/", "search (Library tab)"),
        hrow("r", "reload from the library DB"),
        hrow("?", "toggle this help"),
        hrow("q / Esc", "quit"),
        Line::from(""),
        header("Folder browser (press s):"),
        hrow("↑↓  Enter", "highlight / open a subfolder"),
        hrow("Backspace / h", "go up a level"),
        hrow("s", "scan THIS folder"),
        hrow("t", "type a path instead · Esc cancel"),
        hrow("d  ·  .", "external drives · show hidden"),
        Line::from(""),
        note("Press D to download AI models (~1.6 GB) for a full-AI scan."),
        note("macOS only: one-time `fileid runtime install` for ONNX Runtime."),
    ];
    let w = 64.min(area.width.saturating_sub(4)).max(34);
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centered(area, w, h);
    overlay_bg(f, popup);
    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: true })
        .block(overlay_block("Keys"));
    f.render_widget(p, popup);
}

/// The folder browser overlay (the `s` key). Shows the current folder as a
/// title, a scrollable list of its subdirectories (with a leading `..`), an
/// optional file preview + permission notice, and a one-line key hint. Pure read
/// of `&Browser`.
fn render_browser(f: &mut Frame, browser: &Browser, area: Rect) {
    let w = 78.min(area.width.saturating_sub(2)).max(30);
    let h = 24.min(area.height.saturating_sub(2)).max(10);
    let popup = centered(area, w, h);
    overlay_bg(f, popup);

    let block = overlay_block("Pick a folder to scan");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let files_h: u16 = if browser.files.is_empty() {
        0
    } else {
        (browser.files.len() as u16 + 1).min(7)
    };
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(3),
    ];
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
                " → Scan this folder ",
                Style::default()
                    .fg(Color::Black)
                    .bg(GOLD)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  press ", Style::default().fg(DIM)),
            Span::styled(
                " s ",
                Style::default()
                    .fg(GOLD)
                    .bg(PILL_BG)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        rows[0],
    );

    // (1) Title: the current folder (home-collapsed, tail-truncated) + its own
    // shallow counts, so the user sees what a scan here would pick up.
    let here = truncate(&short(&browser.cwd.to_string_lossy()), inner.width as usize);
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                here,
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                count_summary(&browser.here),
                Style::default().fg(DIM),
            )),
        ])),
        rows[1],
    );

    // (2) The subdirectory list, WINDOWED to the visible rows so counts are
    // computed lazily (one shallow read_dir per on-screen row, cached). Selected
    // row gets the calm gold-bar band, same as the dashboard lists.
    let list_area = rows[2];
    let total = browser.rows.len();
    let vh = (list_area.height as usize).max(1);
    let offset = if total <= vh {
        0
    } else {
        browser.selected.saturating_sub(vh / 2).min(total - vh)
    };
    let end = (offset + vh).min(total);
    let cw = list_area.width.saturating_sub(2) as usize; // 2-char selection gutter
    let items: Vec<ListItem> = browser.rows[offset..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = offset + i == browser.selected;
            let content = match row {
                BrowseRow::Parent => vec![Span::styled(
                    "..   (up a level)",
                    Style::default().fg(LAVENDER),
                )],
                BrowseRow::Dir(p) => {
                    let counts = browser.count_for(p).map(|c| count_summary(&c));
                    dir_row(&format!("{}/", dir_label(p)), counts.as_deref(), cw)
                }
            };
            calm_item(selected, content)
        })
        .collect();
    let list =
        List::new(items).highlight_style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD));
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(browser.selected.saturating_sub(offset)));
    }
    f.render_stateful_widget(list, list_area, &mut state);

    let mut idx = 3;
    if files_h > 0 {
        render_file_preview(f, browser, rows[idx]);
        idx += 1;
    }
    if let Some(notice) = &browser.notice {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(notice, inner.width as usize),
                Style::default().fg(PINK),
            )),
            rows[idx],
        );
        idx += 1;
    }
    let hidden_state = if browser.show_hidden {
        "hidden:on"
    } else {
        "hidden:off"
    };
    let hint =
        format!("↑↓ move · Enter open · ← up · d drives · . {hidden_state} · s scan · Esc cancel");
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(&hint, inner.width as usize),
            Style::default().fg(DIM),
        )),
        rows[idx],
    );
}

/// The dimmed "files here" preview inside the browser: a header with the total,
/// then the file names (image files cyan-flagged), collapsing the overflow into
/// a `+N more` line.
fn render_file_preview(f: &mut Frame, browser: &Browser, area: Rect) {
    let width = area.width as usize;
    let header = format!(
        "Files here ({}{}):",
        browser.files_total,
        if browser.here.capped { "+" } else { "" }
    );
    let mut lines = vec![Line::from(Span::styled(
        header,
        Style::default().fg(DIM).add_modifier(Modifier::BOLD),
    ))];

    let body_cap = (area.height as usize).saturating_sub(1);
    let kept = browser.files.len();
    let need_more = kept > body_cap || browser.files_total > kept || browser.here.capped;
    let show = if need_more {
        body_cap.saturating_sub(1)
    } else {
        body_cap
    };

    for fe in browser.files.iter().take(show) {
        let (marker, color) = if fe.is_image {
            ("▪ ", CYAN)
        } else {
            ("· ", FAINT)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(color)),
            Span::styled(
                truncate(&fe.name, width.saturating_sub(2)),
                Style::default().fg(DIM),
            ),
        ]));
    }
    if need_more {
        let remaining = browser.files_total.saturating_sub(show);
        let plus = if browser.here.capped { "+" } else { "" };
        lines.push(Line::from(Span::styled(
            format!("  … and {remaining}{plus} more"),
            Style::default().fg(FAINT),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// One subdirectory row's CONTENT spans (no selection gutter): `Name/` on the
/// left, its shallow counts dim and right-aligned. `counts == None` means the
/// folder was unreadable.
fn dir_row(name: &str, counts: Option<&str>, usable: usize) -> Vec<Span<'static>> {
    let counts = counts.unwrap_or("· unreadable");
    let cw = counts.chars().count();
    let (name_t, pad) = name_pad(usable, 0, cw, name, 1, 4);
    vec![
        Span::styled(name_t, Style::default().fg(SECONDARY)),
        Span::raw(" ".repeat(pad)),
        Span::styled(counts.to_string(), Style::default().fg(DIM)),
    ]
}

/// Compact one-line tally for a folder: `143 images · 27 files · 12 folders`. A
/// `+` suffix marks a count that hit the shallow walk cap.
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

/// Single-line folder-path prompt (the `t` fallback from the browser): type a
/// path, `Enter`/`Tab` confirm, `Esc` cancel. Shows the typed text + a block
/// cursor, a `~` hint, and an inline error (pink) when a confirm hit a bad path.
fn render_input(f: &mut Frame, app: &App, area: Rect) {
    let w = 66.min(area.width.saturating_sub(4)).max(24);
    let h = 6;
    let popup = centered(area, w, h);
    overlay_bg(f, popup);

    let field_max = w.saturating_sub(5) as usize; // borders + "> " + cursor
    let mut lines = vec![
        Line::from(Span::styled(
            "Folder to scan",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{}\u{2588}", input_tail(&app.input, field_max)),
                Style::default().fg(FG),
            ),
        ]),
    ];
    match &app.input_error {
        Some(err) => lines.push(Line::from(Span::styled(
            truncate(err, w.saturating_sub(2) as usize),
            Style::default().fg(PINK),
        ))),
        None => lines.push(Line::from(Span::styled(
            "~ expands to your home folder",
            Style::default().fg(DIM),
        ))),
    }
    lines.push(Line::from(Span::styled(
        "Enter / Tab confirm · Esc cancel",
        Style::default().fg(FAINT),
    )));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(overlay_block("Scan folder")),
        popup,
    );
}

// ── shared builders ──────────────────────────────────────────────────────────

/// A plain-language context strip under the tabs: left description spans, with an
/// optional right-aligned count (the mockup's `12,481 files`).
fn render_context(f: &mut Frame, area: Rect, left: Vec<Span<'static>>, right: Option<String>) {
    match right {
        Some(r) if area.width > 20 => {
            let rw = r.chars().count() as u16 + 1;
            let parts =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(rw)]).split(area);
            f.render_widget(Paragraph::new(Line::from(left)), parts[0]);
            f.render_widget(
                Paragraph::new(Span::styled(r, Style::default().fg(DIM)))
                    .alignment(Alignment::Right),
                parts[1],
            );
        }
        _ => f.render_widget(Paragraph::new(Line::from(left)), area),
    }
}

/// A list with the calm selection band: the highlighted row gets a solid gold
/// left bar, a gold-tint background, and bold text, while each cell keeps its own
/// accent colour because the highlight patches background only, never foreground.
/// Auto-scrolls the cursor into view via `ListState`.
fn render_calm_list<T>(
    f: &mut Frame,
    area: Rect,
    title: &str,
    data: &[T],
    cursor: usize,
    row: impl Fn(&T) -> Vec<Span<'static>>,
) {
    let len = data.len();
    let viewport = area.height.saturating_sub(2) as usize;
    let cursor = cursor.min(len.saturating_sub(1));
    let offset = if viewport == 0 || cursor < viewport {
        0
    } else {
        cursor - viewport + 1
    };
    let end = (offset + viewport).min(len);
    let items: Vec<ListItem> = data[offset..end]
        .iter()
        .enumerate()
        .map(|(i, d)| calm_item(offset + i == cursor, row(d)))
        .collect();
    let block = focus_block(title);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD));
    let mut state = ListState::default();
    if len > 0 {
        state.select(Some(cursor - offset));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Wrap row CONTENT in the 2-char selection gutter: a gold `▌` bar when selected,
/// a blank otherwise. The bg-only highlight tints the whole band behind it.
fn calm_item(selected: bool, content: Vec<Span<'static>>) -> ListItem<'static> {
    let mut line = Vec::with_capacity(content.len() + 2);
    line.push(if selected {
        Span::styled("▌", Style::default().fg(GOLD))
    } else {
        Span::raw(" ")
    });
    line.push(Span::raw(" "));
    line.extend(content);
    ListItem::new(Line::from(line))
}

/// An empty-state panel: a bold headline, a dim explanation of what the tab does
/// and where its data comes from, and — crucially — a gold call-to-action keycap
/// so "how do I actually fill this?" is answered on every empty screen.
fn render_empty(
    f: &mut Frame,
    area: Rect,
    title: &str,
    head: &str,
    sub: &str,
    action: Option<Vec<Span<'static>>>,
) {
    let mut lines = vec![
        Line::from(Span::styled(
            head.to_string(),
            Style::default().fg(SECONDARY).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(sub.to_string(), Style::default().fg(DIM))),
    ];
    if let Some(action) = action {
        lines.push(Line::from(""));
        lines.push(Line::from(action));
    }
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .block(focus_block(title)),
        area,
    );
}

/// A gold call-to-action line: a black-on-gold key chip + a plain-language
/// instruction. The terminal stand-in for "click here to get started".
fn cta(key: &str, text: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(Color::Black)
                .bg(GOLD)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {text}"), Style::default().fg(FG)),
    ]
}

/// Usable text width inside a calm-list column: panel border (2) + gutter (2).
fn content_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(4)
}

/// A focused panel: muted-gold border + bright-gold title.
fn focus_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GOLD_DIM))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
}

/// A secondary panel: dim border + accent title.
fn titled_block(title: &str, accent: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
}

/// An overlay panel: gold border + gold title on the elevated surface.
fn overlay_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(SURFACE))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
}

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

/// `Clear` resets cells to the terminal default; re-establish the elevated
/// surface bg over the popup so overlays stay legible on light terminals too.
fn overlay_bg(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(Style::default().bg(SURFACE).fg(FG)),
        area,
    );
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

/// Truncate `name` to the room left after the fixed `lead`/`trail` column widths
/// (reserving `gap` between name and trailing column, never below `name_floor`),
/// then return that name plus the run of spaces that pushes `trail` flush right.
fn name_pad(
    total: usize,
    lead: usize,
    trail: usize,
    name: &str,
    gap: usize,
    name_floor: usize,
) -> (String, usize) {
    let name_room = total.saturating_sub(lead + trail + gap).max(name_floor);
    let name_t = truncate(name, name_room);
    let used = lead + name_t.chars().count() + trail;
    let pad = total.saturating_sub(used).max(gap);
    (name_t, pad)
}

/// One Library file row's content spans: a coloured 3-letter kind code, the
/// name, and a right-aligned size.
fn file_row(content_w: usize, fr: &crate::data::FileRow) -> Vec<Span<'static>> {
    let (code, color) = kind_code(&fr.kind);
    let size = human_size(fr.size);
    let sizew = size.chars().count();
    let (name, pad) = name_pad(content_w, 5, sizew, &basename(&fr.path), 1, 4); // 5 = "CODE " field
    vec![
        Span::styled(format!("{code:<4} "), Style::default().fg(color)),
        Span::styled(name, Style::default().fg(SECONDARY)),
        Span::raw(" ".repeat(pad)),
        Span::styled(size, Style::default().fg(DIM)),
    ]
}

/// One People group row: a per-person coloured face dot (cycled by id, the
/// terminal stand-in for the mockup's distinct avatar gradients), the name
/// (faint when still unnamed), and a right-aligned photo/face tally.
fn person_row(content_w: usize, p: &crate::data::PersonRow) -> Vec<Span<'static>> {
    let named = p.name != "Unnamed" && p.name != "Unknown";
    let avatar = [LAVENDER, CYAN, PINK, GOLD, GREEN];
    let dot_color = avatar[(p.id.unsigned_abs() as usize) % avatar.len()];
    let tally = format!("{} files · {} faces", p.files, p.faces);
    let tw = tally.chars().count();
    let (name, pad) = name_pad(content_w, 2, tw, &p.name, 1, 4);
    vec![
        Span::styled("● ", Style::default().fg(dot_color)),
        Span::styled(
            name,
            Style::default().fg(if named { SECONDARY } else { FAINT }),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(tally, Style::default().fg(DIM)),
    ]
}

/// One Cleanup duplicate-set row: a pink `N×` copy count, the file name, and a
/// right-aligned per-copy size.
fn dup_row(content_w: usize, g: &crate::data::DupGroup) -> Vec<Span<'static>> {
    let count = format!("{}×", g.copies);
    let cw = count.chars().count();
    let name = g.paths.first().map_or_else(String::new, |p| basename(p));
    let size = human_size(g.size);
    let sizew = size.chars().count();
    let (name_t, pad) = name_pad(content_w, cw + 1, sizew, &name, 1, 4);
    vec![
        Span::styled(format!("{count} "), Style::default().fg(PINK)),
        Span::styled(name_t, Style::default().fg(SECONDARY)),
        Span::raw(" ".repeat(pad)),
        Span::styled(size, Style::default().fg(DIM)),
    ]
}

/// One Restructure row: a confidence dot, a cyan category tag, the source name,
/// and a right-aligned `→ destination` (the mockup's "Sure? · File · Goes to").
fn plan_row(content_w: usize, m: &crate::data::PlanRow) -> Vec<Span<'static>> {
    let (dot, dot_color) = match m.confidence {
        "auto" => ("●", GREEN),
        "review" => ("◐", GOLD),
        _ => ("●", PINK),
    };
    let cat = truncate(&m.category, 12);
    let catw = cat.chars().count();
    let dest = rel_dest(&m.destination);
    let tailw = 2 + dest.chars().count(); // "→ " + dest
    let fixed = 2 + catw + 1; // dot + category + space
    let (name, pad) = name_pad(content_w, fixed, tailw, &basename(&m.source), 2, 6);
    vec![
        Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
        Span::styled(format!("{cat} "), Style::default().fg(CYAN)),
        Span::styled(name, Style::default().fg(SECONDARY)),
        Span::raw(" ".repeat(pad)),
        Span::styled("→ ", Style::default().fg(DIM)),
        Span::styled(dest, Style::default().fg(LAVENDER)),
    ]
}

/// A dim, uppercase, letter-spaced section header (the mockup's `TAGS`, `PREVIEW`
/// captions).
fn section(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_uppercase(),
        Style::default().fg(FAINT).add_modifier(Modifier::BOLD),
    ))
}

/// A key/value detail row: dim padded key + body-coloured value.
fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<16}"), Style::default().fg(DIM)),
        Span::styled(value.to_string(), Style::default().fg(FG)),
    ])
}

/// A help-overlay key row: gold key + body-coloured description.
fn hrow(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<16}"), Style::default().fg(GOLD)),
        Span::styled(desc.to_string(), Style::default().fg(FG)),
    ])
}

/// `N word` with singular/plural agreement.
fn plural(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// A 3-letter kind code + its accent colour (mockup: IMG cyan, VID pink, PDF
/// gold, DOC lavender).
fn kind_code(kind: &str) -> (&'static str, Color) {
    match kind {
        "image" => ("IMG", CYAN),
        "video" => ("VID", PINK),
        "pdf" => ("PDF", GOLD),
        "doc" => ("DOC", LAVENDER),
        "audio" => ("AUD", LAVENDER),
        "model" => ("3D", DIM),
        _ => ("FILE", DIM),
    }
}

/// Plain-language kind for the detail panel.
fn friendly_kind(kind: &str) -> &'static str {
    match kind {
        "image" => "Photo",
        "video" => "Video",
        "pdf" => "PDF",
        "doc" => "Document",
        "audio" => "Audio",
        "model" => "3D model",
        _ => "File",
    }
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Show the destination relative to its trailing segments (the category folder +
/// filename), which is what matters in the plan view.
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
    fn frame_chunks_splits_header_body_status_keybar() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let c = frame_chunks(area);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].height, 2); // header: brand+tabs row, then the underline divider
        assert_eq!(c[2].height, 1); // status line
        assert_eq!(c[3].height, 1); // always-visible key bar
        assert_eq!(c[1].height, 20); // body gets the rest
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

    #[test]
    fn kind_codes_match_design() {
        assert_eq!(kind_code("image").0, "IMG");
        assert_eq!(kind_code("video").0, "VID");
        assert_eq!(kind_code("pdf").0, "PDF");
        assert_eq!(kind_code("doc").0, "DOC");
        assert_eq!(kind_code("whatever").0, "FILE");
    }

    /// Headless full-frame render via ratatui's TestBackend (no real terminal):
    /// proves the brand/tabs, Library pane, and live file data all paint.
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
        assert!(text.contains("FileID"), "brand/header missing");
        assert!(text.contains("Library"), "Library tab label missing");
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

    /// The UI must paint its OWN dark background so it's legible on a
    /// light-background terminal. Every cell of the body region must carry the
    /// brand near-black `BG` (never the terminal default `Color::Reset`), and a
    /// known label (the gold brand accent) must render in a readable foreground.
    ///
    /// We sweep the body chunk specifically on the first-run welcome screen
    /// (loaded-but-no-DB, Library tab): it uses only foreground accents — no
    /// selected-row tint, no keycap chips — so every body cell must carry `BG`.
    #[test]
    fn paints_dark_background_and_gold_accent() {
        use crate::app::App;
        use crate::data::{LoadMsg, Snapshot};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (w, h) = (80u16, 24u16);
        let mut app = App::new("/tmp/x.sqlite".into());
        // Settle the loader to an empty, not-yet-created library so the welcome
        // screen renders (loading=false, db_exists=false) — the all-BG body.
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: false,
            ..Snapshot::default()
        })));
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();

        // Body chunk = rows [2, h-2): every cell painted with the brand dark bg,
        // never left on the terminal's default (which would vanish on light bg).
        let body = frame_chunks(Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        })[1];
        for y in body.top()..body.bottom() {
            for x in 0..w {
                let bg = buf[(x, y)].bg;
                assert_ne!(
                    bg,
                    Color::Reset,
                    "cell ({x},{y}) left on terminal-default bg"
                );
                assert_eq!(bg, BG, "cell ({x},{y}) not painted with brand dark bg");
            }
        }

        // The gold brand accent renders somewhere (the `FileID` brand / tabs),
        // proving a known label paints in a high-contrast fg on the dark bg.
        let has_gold = (0..h).any(|y| (0..w).any(|x| buf[(x, y)].fg == GOLD));
        assert!(has_gold, "gold brand accent not rendered in any cell");
    }

    /// The active tab is underlined in bright gold on the header's divider row
    /// (row 1) — the terminal stand-in for the mockup's 2px gold underline.
    #[test]
    fn active_tab_gets_a_gold_underline() {
        use crate::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let app = App::new("/tmp/x.sqlite".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();

        // Row 1 must hold at least one gold `─` (under "1 Library", the default
        // active tab), and the rest of the rule is the dim divider colour.
        let gold_rule = (0..80).any(|x| buf[(x, 1)].fg == GOLD && buf[(x, 1)].symbol() == "─");
        assert!(gold_rule, "no gold underline on the active tab");
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

    /// The always-visible key bar shows the active tab's actions. On Library it
    /// advertises scan + search; switching to People drops `search`. The keys are
    /// rendered as dark pills (no brackets), with `for all keys` pinned right.
    #[test]
    fn key_bar_advertises_tab_actions_and_fits_80_cols() {
        use crate::app::{App, Tab};

        let app = App::new("/tmp/x.sqlite".into());
        let lib = frame_text(80, 24, &app);
        // The bottom row (the key bar) must carry the headline action labels.
        let bar = lib.lines().last().unwrap_or("");
        assert!(bar.contains("scan"), "key bar missing scan: {bar:?}");
        assert!(
            bar.contains("search"),
            "Library bar missing search: {bar:?}"
        );
        assert!(bar.contains("quit"), "key bar missing quit: {bar:?}");
        assert!(
            bar.contains("for all keys"),
            "key bar missing the help tail: {bar:?}"
        );
        // Everything fits an 80-col row: no glyph is lost off the edge.
        assert!(
            bar.trim_end().chars().count() <= 80,
            "key bar overflows 80 cols: {bar:?}"
        );

        // People tab: search is Library-only, so it drops from the bar.
        let mut app2 = App::new("/tmp/x.sqlite".into());
        app2.tab = Tab::People;
        let ppl = frame_text(80, 24, &app2);
        let bar = ppl.lines().last().unwrap_or("");
        assert!(
            bar.contains("scan"),
            "key bar missing scan off-Library: {bar:?}"
        );
        assert!(
            !bar.contains("search"),
            "search hint must be Library-only: {bar:?}"
        );
    }

    /// The folder browser overlay paints the current folder, its subdirectories
    /// (with a `..` row), the file preview, and the one-line key hint.
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

        assert!(
            text.contains("Pick a folder to scan"),
            "browser title missing"
        );
        assert!(text.contains("Scan this folder"), "scan affordance missing");
        assert!(text.contains("Pictures/"), "subdir Pictures not listed");
        assert!(text.contains("Documents/"), "subdir Documents not listed");
        assert!(text.contains("folders"), "per-folder/own counts missing");
        assert!(text.contains("Files here"), "file preview header missing");
        assert!(text.contains("photo.png"), "preview file not listed");
        assert!(text.contains(".."), "the up-a-level row is missing");
        // The hint line advertises the new drives jump + hidden toggle (with its
        // current state), alongside the scan/cancel affordances.
        assert!(text.contains("d drives"), "browser drives hint missing");
        assert!(
            text.contains("hidden:off"),
            "browser hidden-toggle hint missing"
        );
        assert!(text.contains("s scan"), "browser scan hint missing");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// At a tight 80×24 the browser overlay still shows the new drives jump +
    /// hidden-toggle hints un-clipped (the in-popup hint truncates gracefully
    /// rather than overflowing).
    #[test]
    fn browser_overlay_hints_render_at_80_cols() {
        use crate::app::{App, Browser};

        let mut app = App::new("/tmp/x.sqlite".into());
        app.browser = Some(Browser::open(std::env::temp_dir()));
        let text = frame_text(80, 24, &app);
        assert!(
            text.contains("d drives"),
            "drives hint clipped/missing at 80 cols"
        );
        assert!(
            text.contains("hidden:off"),
            "hidden hint clipped/missing at 80 cols"
        );
        assert!(
            text.contains("Esc cancel"),
            "cancel hint clipped at 80 cols"
        );
    }

    /// FEATURE 2: dotfiles are hidden by default, and the browser hint reflects
    /// the toggle state; pressing `.` flips it to `hidden:on`.
    #[test]
    fn browser_dotfile_toggle_reflects_state_in_hint() {
        use crate::app::{App, Browser};
        use crossterm::event::{KeyCode, KeyModifiers};

        let base = std::env::temp_dir().join(format!("fileid-ui-dot-{}", std::process::id()));
        std::fs::create_dir_all(base.join("Shown")).unwrap();
        std::fs::create_dir_all(base.join(".cache")).unwrap();
        std::fs::write(base.join(".env"), "x").unwrap();

        let mut app = App::new("/tmp/x.sqlite".into());
        app.browser = Some(Browser::open(base.clone()));
        // Default: the dot-entries are filtered, and the hint reads `hidden:off`.
        let text = frame_text(100, 30, &app);
        assert!(
            text.contains("hidden:off"),
            "default state must read hidden:off"
        );
        assert!(
            !text.contains(".cache/"),
            "hidden subdir must not render by default"
        );

        // Press `.` → hidden entries reveal, and the hint flips to `hidden:on`.
        app.on_key(KeyCode::Char('.'), KeyModifiers::NONE);
        let text = frame_text(100, 30, &app);
        assert!(
            text.contains("hidden:on"),
            "toggled state must read hidden:on"
        );
        assert!(
            text.contains(".cache/"),
            "hidden subdir renders once toggled on"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// FEATURE 3: the Settings panel surfaces the model-download action, and the
    /// always-visible key bar advertises `D` — fitting an 80-col row.
    #[test]
    fn settings_tab_shows_model_download_action_and_key() {
        use crate::app::{App, Tab};
        use crate::data::{LoadMsg, Snapshot};

        let mut app = App::new("/tmp/x.sqlite".into());
        app.tab = Tab::Settings;
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: true,
            ..Snapshot::default()
        })));
        let text = frame_text(80, 30, &app);
        assert!(
            text.contains("Download all AI models"),
            "Settings panel missing the download action"
        );
        assert!(
            text.contains("get AI models"),
            "Settings key bar missing the model-download hint"
        );
        let bar = text.lines().last().unwrap_or("");
        assert!(
            bar.trim_end().chars().count() <= 80,
            "Settings key bar overflows 80 cols: {bar:?}"
        );
    }

    /// Empty-scratch start: a loaded-but-empty library (db_exists, zero rows on
    /// every tab) plus an open folder browser must render at any terminal size —
    /// including tiny ones that collapse panels to zero area — without panicking
    /// (no out-of-range list index, no `len - 1` underflow, no bad slice range).
    #[test]
    fn renders_every_tab_and_browser_empty_without_panic() {
        use crate::app::{App, Browser, Tab};
        use crate::data::{LoadMsg, Snapshot};

        // A few sizes: realistic, narrow, and pathologically small.
        for (w, h) in [(100u16, 30u16), (80, 24), (20, 6), (4, 3), (1, 1)] {
            for tab in Tab::ALL {
                let mut app = App::new("/tmp/x.sqlite".into());
                app.tab = tab;
                // Exercise the standing models-missing banner at every size too.
                app.missing_models = vec!["MobileCLIP".to_string()];
                // db_exists=true with empty vecs → the per-tab empty branches run
                // (not the welcome screen), exercising the empty-list renderers.
                app.apply_load(LoadMsg::Done(Box::new(Snapshot {
                    db_exists: true,
                    ..Snapshot::default()
                })));
                let _ = frame_text(w, h, &app); // must not panic

                // …and the same with the folder browser open over it.
                app.browser = Some(Browser::open(std::env::temp_dir()));
                let _ = frame_text(w, h, &app); // must not panic
            }
        }
    }

    /// The standing "models missing" banner renders above EVERY tab's body (so a
    /// fresh install can't miss the prompt to press `D`), and the Settings panel —
    /// `D`'s on-screen home — is reachable even before the first scan creates the
    /// library DB. Previously the welcome screen masked every tab on a fresh DB,
    /// so Settings (and the download prompt) were unreachable.
    #[test]
    fn models_missing_banner_and_settings_reachable_on_a_fresh_library() {
        use crate::app::{App, Tab};
        use crate::data::{LoadMsg, Snapshot};

        for tab in Tab::ALL {
            let mut app = App::new("/tmp/x.sqlite".into());
            app.missing_models = vec!["CLIP image encoder".to_string()];
            // Resolved-but-not-yet-created library (fresh scratch): db_exists=false.
            app.apply_load(LoadMsg::Done(Box::new(Snapshot {
                db_exists: false,
                ..Snapshot::default()
            })));
            app.tab = tab;
            let text = frame_text(100, 30, &app);
            assert!(
                text.contains("AI models not installed"),
                "{tab:?}: the standing models-missing banner must render above the body",
            );
            // The banner states the REAL size of the TUI's (non-VLM) model set,
            // never the false ~25 GB that included the Deep-Analyze VLMs.
            assert!(
                text.contains("~1.6 GB"),
                "{tab:?}: banner must state the real ~1.6 GB size"
            );
            assert!(
                !text.contains("25 GB"),
                "{tab:?}: banner must not claim the VLM ~25 GB total"
            );
        }

        // Settings is reachable on a fresh library: its download action shows even
        // with db_exists=false, while Library still shows the first-run welcome.
        let mut app = App::new("/tmp/x.sqlite".into());
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: false,
            ..Snapshot::default()
        })));
        app.tab = Tab::Settings;
        assert!(
            frame_text(100, 30, &app).contains("Download all AI models"),
            "Settings (the D prompt) must be reachable before the first scan",
        );
        app.tab = Tab::Library;
        assert!(
            frame_text(100, 30, &app).contains("Welcome to FileID"),
            "Library keeps the first-run welcome screen",
        );
    }

    /// While a download is in flight the banner becomes a real progress gauge:
    /// the `Installing AI models…` title, the overall percent, the live label,
    /// and a visibly gold-filled bar (full-block cells in the gold accent).
    #[test]
    fn models_banner_shows_download_gauge() {
        use crate::app::{App, DownloadState};
        use crate::data::{LoadMsg, Snapshot};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new("/tmp/x.sqlite".into());
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: false,
            ..Snapshot::default()
        })));
        app.download = Some(DownloadState {
            percent: 62,
            label: "arcface · 182/271 MB · 3.4 MB/s · model 2/9".to_string(),
            done: false,
        });

        let text = frame_text(100, 30, &app);
        assert!(text.contains("Installing AI models"), "gauge title missing");
        assert!(text.contains("62%"), "gauge percent label missing");
        assert!(text.contains("arcface"), "gauge live label missing");

        // The bar is actually FILLED in gold — proof it renders, not just labels.
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let gold_fill = (0..30)
            .any(|y| (0..100).any(|x| buf[(x, y)].fg == GOLD && buf[(x, y)].symbol() == "█"));
        assert!(gold_fill, "gauge must paint a gold-filled bar");
    }

    /// The install gauge must render at ANY terminal size — including ones that
    /// collapse the band to near-zero — without panicking (no bad `Layout` split,
    /// no `Gauge::percent` overflow at the 0/47/100 boundaries).
    #[test]
    fn download_gauge_renders_at_every_size_without_panic() {
        use crate::app::{App, DownloadState};
        use crate::data::{LoadMsg, Snapshot};

        for (w, h) in [(100u16, 30u16), (80, 24), (20, 6), (4, 3), (1, 1)] {
            for (percent, done) in [(0u16, false), (47, false), (100, true)] {
                let mut app = App::new("/tmp/x.sqlite".into());
                app.apply_load(LoadMsg::Done(Box::new(Snapshot {
                    db_exists: true,
                    ..Snapshot::default()
                })));
                app.download = Some(DownloadState {
                    percent,
                    label: "arcface · 182/271 MB".to_string(),
                    done,
                });
                let _ = frame_text(w, h, &app); // must not panic
            }
        }
    }

    /// When the final `PROGRESS\t100\tdone` lands the gauge flips to its brief
    /// green success state telling the user they can now scan with full AI.
    #[test]
    fn models_banner_gauge_done_state_invites_a_scan() {
        use crate::app::{App, DownloadState};
        use crate::data::{LoadMsg, Snapshot};

        let mut app = App::new("/tmp/x.sqlite".into());
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: false,
            ..Snapshot::default()
        })));
        app.download = Some(DownloadState {
            percent: 100,
            label: "done".to_string(),
            done: true,
        });
        let text = frame_text(100, 30, &app);
        assert!(
            text.contains("AI models installed"),
            "done gauge must confirm install"
        );
        assert!(
            text.contains("scan with full AI"),
            "done gauge must invite a scan"
        );
        assert!(text.contains("100%"), "done gauge shows 100%");
    }

    /// A failed action (e.g. the scan pre-flight bailing on missing models) wears
    /// a distinct ⚠ on the status line, never the success ✓.
    #[test]
    fn status_line_marks_errors_distinctly() {
        use crate::app::App;
        use crate::data::LoadMsg;

        let mut app = App::new("/tmp/x.sqlite".into());
        app.apply_load(LoadMsg::Error(
            "scan needs AI models not installed".to_string(),
        ));
        let text = frame_text(100, 30, &app);
        assert!(
            text.contains("⚠"),
            "an errored status must show the ⚠ marker"
        );
        assert!(
            text.contains("scan needs AI models not installed"),
            "the error text persists"
        );
    }

    /// Every tab's empty state now answers "how do I fill this?" instead of
    /// showing a blank panel: Library prompts a scan, a no-hit search shows a
    /// distinct no-match state, and People/Cleanup/Restructure each explain
    /// themselves and point at `s`.
    #[test]
    fn empty_states_explain_the_tab_and_offer_a_next_step() {
        use crate::app::{App, Tab};
        use crate::data::{LoadMsg, Snapshot};

        // A loaded-but-empty library (db_exists, zero rows) runs the per-tab
        // empty branches rather than the first-run welcome screen.
        let load = || {
            let mut app = App::new("/tmp/x.sqlite".into());
            app.apply_load(LoadMsg::Done(Box::new(Snapshot {
                db_exists: true,
                ..Snapshot::default()
            })));
            app
        };

        // Library: a headline + the s-to-scan call-to-action.
        let mut lib = load();
        let t = frame_text(100, 30, &lib);
        assert!(
            t.contains("No files yet."),
            "Library empty headline missing"
        );
        assert!(
            t.contains("pick a folder and scan it"),
            "Library empty CTA missing"
        );

        // Library no-match: a search that hits nothing is a DISTINCT state.
        lib.search = "zzz-no-such-thing".to_string();
        let t = frame_text(100, 30, &lib);
        assert!(
            t.contains("No matches."),
            "Library no-match headline missing"
        );
        assert!(
            t.contains("clear the search"),
            "Library no-match CTA missing"
        );

        // People / Cleanup / Restructure: each explains itself + offers a step.
        for (tab, headline, cta_text) in [
            (Tab::People, "No people yet.", "detect & group faces"),
            (Tab::Cleanup, "No duplicates found.", "check for duplicates"),
            (
                Tab::Restructure,
                "No moves to suggest yet.",
                "this plan fills in",
            ),
        ] {
            let mut app = load();
            app.tab = tab;
            let t = frame_text(100, 30, &app);
            assert!(t.contains(headline), "{tab:?}: empty headline missing");
            assert!(
                t.contains(cta_text),
                "{tab:?}: empty-state call-to-action missing"
            );
        }
    }

    /// Settings surfaces a live model-status line: gold "not installed — …"
    /// naming the missing weights, flipping to green "all installed" when present.
    #[test]
    fn settings_shows_live_model_status() {
        use crate::app::{App, Tab};
        use crate::data::{LoadMsg, Snapshot};

        let mut app = App::new("/tmp/x.sqlite".into());
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: true,
            ..Snapshot::default()
        })));
        app.tab = Tab::Settings;

        app.missing_models = vec!["arcface".to_string(), "MobileCLIP".to_string()];
        let t = frame_text(90, 40, &app);
        assert!(
            t.contains("not installed"),
            "Settings must report missing models"
        );
        assert!(
            t.contains("arcface"),
            "Settings must name the missing models"
        );

        app.missing_models.clear();
        let t = frame_text(90, 40, &app);
        assert!(
            t.contains("all required models installed"),
            "Settings must confirm when all models are present",
        );
    }
}
