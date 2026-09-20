//! Rendering.
//!
//! The two diff panes render the same slice of the same row list, so they stay
//! aligned no matter what either side contains.

use ratatui::layout::{Alignment, Constraint, Direction, Layout as L, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Layout, Overlay};
use crate::diff::{Cell, Row, RowKind};
use crate::keys::Context;
use crate::picker::{Picker, Target};
use crate::theme::Theme;
use crate::tree::NodeKind;

/// Matches the user's `set tabstop=4`.
const TABSTOP: usize = 4;

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let chunks = L::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());
    let (body, status) = (chunks[0], chunks[1]);

    let body = if app.show_tree {
        let cols = L::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(app.tree_width.min(body.width.saturating_sub(20))),
                Constraint::Min(10),
            ])
            .split(body);
        draw_tree(f, app, theme, cols[0]);
        cols[1]
    } else {
        body
    };

    draw_diff(f, app, theme, body);
    draw_status(f, app, theme, status);

    match &app.overlay {
        Some(Overlay::Help { scroll }) => draw_help(f, app, theme, *scroll),
        Some(Overlay::Picker(_)) => draw_picker(f, app, theme),
        Some(Overlay::Prompt(_)) | None => {}
    }
}

// ------------------------------------------------------------------- tree

fn draw_tree(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let focused = app.focus == Context::Tree;
    let filter = app.tree.filter().to_string();
    let title = if filter.is_empty() {
        format!(" changed ({}) ", app.files.len())
    } else {
        format!(" filter: {filter} ")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.pane_border(focused))
        .title_top(Line::from(title).style(theme.title));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let height = inner.height as usize;
    // Keep the cursor inside the window.
    let selected = app.tree.selected;
    if selected < app.tree_offset {
        app.tree_offset = selected;
    } else if height > 0 && selected >= app.tree_offset + height {
        app.tree_offset = selected + 1 - height;
    }
    app.tree_offset = app
        .tree_offset
        .min(app.tree.nodes.len().saturating_sub(height));

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, node) in app
        .tree
        .nodes
        .iter()
        .enumerate()
        .skip(app.tree_offset)
        .take(height)
    {
        let mut spans = Vec::new();
        // neo-tree's indent guides: a rail for every ancestor that still has
        // siblings below it, then a tee or an elbow for this row. Top-level
        // rows carry no connector, so their level contributes no rail either.
        let mut guide = String::new();
        for &ancestor_last in node.guides.iter().skip(1) {
            guide.push_str(if ancestor_last { "  " } else { "\u{2502} " });
        }
        if node.depth > 0 {
            guide.push_str(if node.last_child { "\u{2514} " } else { "\u{251c} " });
        }
        spans.push(Span::styled(guide, theme.dim));

        match node.kind {
            NodeKind::Dir => {
                spans.push(Span::styled(
                    if node.expanded { "▾ " } else { "▸ " },
                    theme.dir,
                ));
                spans.push(Span::styled(node.name.clone(), theme.dir));
                spans.push(Span::styled(format!(" {}", node.count), theme.dim));
            }
            NodeKind::File(_) => {
                let badge = node.status.map_or(" ", |s| s.badge());
                let style = node.status.map_or(theme.dim, |s| theme.badge(s));
                spans.push(Span::styled(format!("{badge} "), style));
                spans.push(Span::styled(node.name.clone(), theme.text));
            }
        }

        let mut line = Line::from(spans);
        if i == selected {
            line = line.style(if focused {
                theme.selection
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            });
        }
        lines.push(line);
    }

    if app.tree.nodes.is_empty() {
        lines.push(Line::styled("  no changes", theme.dim));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------- diff

fn draw_diff(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let focused = app.focus == Context::Diff;
    let path = app
        .current_file()
        .map_or_else(|| "no file".to_string(), |f| f.path.clone());

    let (left_label, right_label) = if app.swapped {
        (app.spec.new_label(), app.spec.old_label())
    } else {
        (app.spec.old_label(), app.spec.new_label())
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.pane_border(focused))
        .title_bottom(Line::from(format!(" {path} ")).style(theme.title))
        .title_bottom(
            Line::from(format!(" {} ", app.summary()))
                .style(theme.dim)
                .alignment(Alignment::Right),
        );

    if app.layout == Layout::Split {
        block = block
            .title_top(Line::from(format!(" {left_label} ")).style(theme.dim))
            .title_top(
                Line::from(format!(" {right_label} "))
                    .style(theme.dim)
                    .alignment(Alignment::Right),
            );
    } else {
        block = block.title_top(
            Line::from(format!(" {left_label} → {right_label} ")).style(theme.dim),
        );
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    app.viewport = inner.height as usize;
    app.scroll_into_view();

    let Some(doc) = &app.doc else {
        f.render_widget(
            Paragraph::new(Line::styled("  nothing to show", theme.dim)),
            inner,
        );
        return;
    };

    if doc.rows.is_empty() {
        let msg = match doc.body {
            crate::diff::Body::Binary => "  binary file -- no textual diff",
            _ => "  no changes in this file",
        };
        f.render_widget(Paragraph::new(Line::styled(msg, theme.dim)), inner);
        return;
    }

    let visible: Vec<(usize, &Row)> = doc
        .rows
        .iter()
        .enumerate()
        .skip(app.offset)
        .take(inner.height as usize)
        .collect();

    let gutter = gutter_width(doc);
    let hits = &app.find_query().to_string();

    match app.layout {
        Layout::Unified => {
            let lines = visible
                .iter()
                .map(|(i, row)| unified_line(row, *i == app.cursor, gutter, app, theme, hits))
                .collect::<Vec<_>>();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Layout::Split => {
            let cols = L::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Length(1),
                    Constraint::Percentage(50),
                ])
                .split(inner);

            let (left_side, right_side) = if app.swapped {
                (Side::New, Side::Old)
            } else {
                (Side::Old, Side::New)
            };

            for (area, side) in [(cols[0], left_side), (cols[2], right_side)] {
                let lines = visible
                    .iter()
                    .map(|(i, row)| {
                        side_line(
                            row,
                            side,
                            *i == app.cursor,
                            gutter,
                            area.width as usize,
                            app,
                            theme,
                            hits,
                        )
                    })
                    .collect::<Vec<_>>();
                f.render_widget(Paragraph::new(lines), area);
            }

            // The divider between the panes.
            let rule: Vec<Line> = (0..cols[1].height)
                .map(|_| Line::styled("│", theme.border))
                .collect();
            f.render_widget(Paragraph::new(rule), cols[1]);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

fn gutter_width(doc: &crate::diff::Document) -> usize {
    let max = doc
        .rows
        .iter()
        .filter_map(|r| {
            let a = r.old.as_ref().map(|c| c.number).unwrap_or(0);
            let b = r.new.as_ref().map(|c| c.number).unwrap_or(0);
            Some(a.max(b))
        })
        .max()
        .unwrap_or(1);
    max.to_string().len().max(3)
}

#[allow(clippy::too_many_arguments)]
fn side_line<'a>(
    row: &Row,
    side: Side,
    is_cursor: bool,
    gutter: usize,
    width: usize,
    app: &App,
    theme: &Theme,
    query: &str,
) -> Line<'a> {
    if row.kind == RowKind::Separator {
        return separator_line(width, theme);
    }

    let cell = match side {
        Side::Old => row.old.as_ref(),
        Side::New => row.new.as_ref(),
    };

    let (base, emph_style, sign) = match (row.kind, side) {
        (RowKind::Context, _) => (theme.context, theme.context, ' '),
        (RowKind::Removed, Side::Old) | (RowKind::Changed, Side::Old) => {
            (theme.removed, theme.removed_word, '-')
        }
        (RowKind::Added, Side::New) | (RowKind::Changed, Side::New) => {
            (theme.added, theme.added_word, '+')
        }
        // This side has no line at all.
        _ => (theme.filler, theme.filler, ' '),
    };

    let mut spans = Vec::new();
    spans.push(Span::styled(
        if is_cursor { "▌" } else { " " },
        theme.border_focus,
    ));

    match cell {
        None => {
            // Fill the row so the block of colour makes the gap obvious.
            spans.push(Span::styled(" ".repeat(width.saturating_sub(1)), theme.filler));
        }
        Some(cell) => {
            spans.push(Span::styled(
                format!("{:>gutter$} ", cell.number),
                theme.gutter,
            ));
            spans.push(Span::styled(sign.to_string(), base));
            let text_width = width.saturating_sub(gutter + 3);
            spans.extend(text_spans(
                cell, base, emph_style, theme, query, app.hscroll, text_width,
            ));
        }
    }

    let mut line = Line::from(spans);
    if is_cursor {
        line = line.style(Style::new().add_modifier(Modifier::BOLD));
    }
    line
}

fn unified_line<'a>(
    row: &Row,
    is_cursor: bool,
    gutter: usize,
    app: &App,
    theme: &Theme,
    query: &str,
) -> Line<'a> {
    if row.kind == RowKind::Separator {
        return separator_line(60, theme);
    }

    // `diff::unify` has already split changed rows in two, so exactly one side
    // carries the text of any non-context row.
    let (cell, base, emph, sign) = match row.kind {
        RowKind::Context => (row.new.as_ref(), theme.context, theme.context, ' '),
        RowKind::Added | RowKind::Changed => {
            (row.new.as_ref(), theme.added, theme.added_word, '+')
        }
        RowKind::Removed => (row.old.as_ref(), theme.removed, theme.removed_word, '-'),
        RowKind::Separator => unreachable!(),
    };

    let mut spans = vec![Span::styled(
        if is_cursor { "\u{258c}" } else { " " },
        theme.border_focus,
    )];

    let Some(cell) = cell else {
        return Line::from(spans);
    };

    // Two gutters: the line's number on the old side, then on the new side.
    // A line that exists on only one side leaves the other column blank.
    let blank = " ".repeat(gutter);
    let old_no = row
        .old
        .as_ref()
        .map_or_else(|| blank.clone(), |c| format!("{:>gutter$}", c.number));
    let new_no = row
        .new
        .as_ref()
        .map_or(blank, |c| format!("{:>gutter$}", c.number));
    spans.push(Span::styled(format!("{old_no} {new_no} "), theme.gutter));
    spans.push(Span::styled(sign.to_string(), base));
    spans.extend(text_spans(
        cell,
        base,
        emph,
        theme,
        query,
        app.hscroll,
        usize::MAX,
    ));

    let mut line = Line::from(spans);
    if is_cursor {
        line = line.style(Style::new().add_modifier(Modifier::BOLD));
    }
    line
}

fn separator_line<'a>(width: usize, theme: &Theme) -> Line<'a> {
    Line::styled("·".repeat(width.max(1)), theme.separator)
}

/// Split a cell into styled runs, expand tabs, apply horizontal scroll and
/// clip to `width` columns.
fn text_spans<'a>(
    cell: &Cell,
    base: Style,
    emph_style: Style,
    theme: &Theme,
    query: &str,
    hscroll: usize,
    width: usize,
) -> Vec<Span<'a>> {
    // Search hits outrank word-level emphasis, so mark them last.
    let mut marks: Vec<(usize, usize, Style)> = cell
        .emphasis
        .iter()
        .map(|&(s, e)| (s, e, emph_style))
        .collect();
    if !query.is_empty() {
        for (s, e) in find_all(&cell.text, query) {
            marks.push((s, e, theme.match_hit));
        }
    }
    // Later marks win where they overlap.
    marks.sort_by_key(|m| m.0);

    let runs = split_runs(&cell.text, base, &marks);
    layout(runs, hscroll, width)
}

/// Case-insensitive when the query is all lowercase, like vim's `smartcase`.
fn find_all(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = !query.chars().any(char::is_uppercase);
    let hay = if fold { text.to_lowercase() } else { text.into() };
    let needle = if fold { query.to_lowercase() } else { query.into() };
    // Folding can change byte length for non-ASCII, which would misplace the
    // highlight; skip it rather than paint the wrong columns.
    if hay.len() != text.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let s = from + i;
        out.push((s, s + needle.len()));
        from = s + needle.len().max(1);
    }
    out
}

/// Cut `text` into `(slice, style)` runs at every mark boundary.
///
/// Cut points are snapped down to a character boundary rather than discarded:
/// a mark that lands mid-character costs a little highlight precision, but
/// dropping the span would blank the whole line instead.
fn split_runs<'t>(
    text: &'t str,
    base: Style,
    marks: &[(usize, usize, Style)],
) -> Vec<(&'t str, Style)> {
    if marks.is_empty() {
        return vec![(text, base)];
    }
    let floor = |i: usize| {
        let mut i = i.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };

    let mut cuts: Vec<usize> = vec![0, text.len()];
    for &(s, e, _) in marks {
        cuts.push(floor(s));
        cuts.push(floor(e));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        // The last mark covering this span wins.
        let style = marks
            .iter()
            .filter(|&&(ms, me, _)| ms <= s && e <= me)
            .next_back()
            .map_or(base, |&(_, _, st)| st);
        out.push((&text[s..e], style));
    }
    out
}

/// Expand tabs, drop the first `hscroll` columns, and stop at `width`.
///
/// Everything here is measured in columns, not bytes: a tab reaches the next
/// multiple of [`TABSTOP`] counted from the start of the real line, and a
/// double-width character occupies two columns.
fn layout<'a>(runs: Vec<(&str, Style)>, hscroll: usize, width: usize) -> Vec<Span<'a>> {
    let mut out: Vec<Span> = Vec::new();
    // Column in the fully expanded line, and columns actually written.
    let (mut column, mut emitted) = (0usize, 0usize);

    for (text, style) in runs {
        let mut buf = String::new();
        for ch in text.chars() {
            if ch == '\t' {
                // Emitted one column at a time so a horizontal scroll can land
                // in the middle of a tab.
                let stop = TABSTOP - (column % TABSTOP);
                for _ in 0..stop {
                    if column >= hscroll {
                        if emitted >= width {
                            flush(&mut out, &mut buf, style);
                            return out;
                        }
                        buf.push(' ');
                        emitted += 1;
                    }
                    column += 1;
                }
                continue;
            }

            let (rendered, w) = if ch.is_control() {
                ("\u{b7}".to_string(), 1)
            } else {
                (ch.to_string(), UnicodeWidthChar::width(ch).unwrap_or(1))
            };

            if column >= hscroll {
                // Never write a wide character that would straddle the edge.
                if emitted + w > width {
                    flush(&mut out, &mut buf, style);
                    return out;
                }
                buf.push_str(&rendered);
                emitted += w;
            }
            column += w;
        }
        flush(&mut out, &mut buf, style);
    }
    out
}

fn flush<'a>(out: &mut Vec<Span<'a>>, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        out.push(Span::styled(std::mem::take(buf), style));
    }
}

// ----------------------------------------------------------------- status

fn draw_status(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // A prompt takes over the status line, exactly as `/` does in vim.
    if let Some(Overlay::Prompt(p)) = &app.overlay {
        let text = format!("{}{}", p.label(), p.input);
        f.render_widget(
            Paragraph::new(Line::styled(text, theme.prompt)).style(theme.status),
            area,
        );
        return;
    }

    if let Some(msg) = &app.message {
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {msg}"), theme.warn)).style(theme.status),
            area,
        );
        return;
    }

    let mode = match app.focus {
        Context::Tree => "TREE",
        Context::Diff => "DIFF",
    };
    let position = app.doc.as_ref().map_or_else(String::new, |d| {
        if d.rows.is_empty() {
            String::new()
        } else {
            format!("{}/{}", app.cursor + 1, d.rows.len())
        }
    });

    let mut spans = vec![
        Span::styled(format!(" {mode} "), theme.status_accent),
        Span::styled(format!(" {} ", app.repo.head_label()), theme.status),
        Span::styled(
            format!("│ {} ", app.current_file().map_or("-", |f| f.path.as_str())),
            theme.status,
        ),
        Span::styled(format!("│ {position} "), theme.status),
        Span::styled(format!("│ {} ", app.summary()), theme.status),
    ];
    if let Some((at, total)) = app.hunk_position() {
        spans.push(Span::styled(
            format!("│ hunk {at}/{total} "),
            theme.status,
        ));
    }
    if !app.find_query().is_empty() {
        spans.push(Span::styled(
            format!("│ /{} {} ", app.find_query(), app.find_matches().len()),
            theme.status,
        ));
    }
    if app.full_context {
        spans.push(Span::styled("│ whole file ", theme.status));
    }
    if !app.quickfix.is_empty() {
        spans.push(Span::styled(
            format!("│ qf {}/{} ", app.quickfix_at + 1, app.quickfix.len()),
            theme.status,
        ));
    }
    spans.push(Span::styled("│ ? help ", theme.status));

    let pending = app.pending_label();
    if !pending.is_empty() {
        spans.push(Span::styled(format!("│ {pending} "), theme.status_accent));
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.status),
        area,
    );
}

// ----------------------------------------------------------------- picker

/// Telescope's default geometry: 90% wide, 85% tall, preview on the right.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn draw_picker(f: &mut Frame, app: &mut App, theme: &Theme) {
    let area = centered(f.area(), 90, 85);
    f.render_widget(Clear, area);

    let cols = L::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    // Prompt on top, results below -- the user's `prompt_position = "top"`.
    let rows = L::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(cols[0]);

    let (prompt, title, count, selected_target) = {
        let Some(Overlay::Picker(p)) = &app.overlay else {
            return;
        };
        (
            p.prompt.clone(),
            p.title.clone(),
            p.items().len(),
            p.selected_target().cloned(),
        )
    };

    let prompt_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_focus);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", theme.prompt),
            Span::styled(prompt, theme.text),
            Span::styled("▏", theme.prompt),
        ]))
        .block(prompt_block),
        rows[0],
    );

    let results_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title_top(Line::from(format!(" {title} ")).style(theme.title))
        .title_bottom(
            Line::from(format!(" {count} "))
                .style(theme.dim)
                .alignment(Alignment::Right),
        );
    let results_inner = results_block.inner(rows[1]);
    f.render_widget(results_block, rows[1]);

    let height = results_inner.height as usize;
    if let Some(Overlay::Picker(p)) = &mut app.overlay {
        p.scroll_into_view(height);
    }
    let Some(Overlay::Picker(p)) = &app.overlay else {
        return;
    };
    f.render_widget(
        Paragraph::new(result_lines(p, height, results_inner.width as usize, theme)),
        results_inner,
    );

    draw_preview(f, app, theme, cols[1], selected_target);
}

fn result_lines<'a>(p: &Picker, height: usize, width: usize, theme: &Theme) -> Vec<Line<'a>> {
    p.items()
        .iter()
        .enumerate()
        .skip(p.offset)
        .take(height)
        .map(|(i, item)| {
            let selected = i == p.selected;
            let mut spans = vec![Span::styled(
                if selected { "▌ " } else { "  " },
                theme.border_focus,
            )];

            // Highlight the characters the fuzzy matcher landed on.
            let mut chars = item.candidate.display.chars().enumerate().peekable();
            let mut buf = String::new();
            let mut buf_hit = false;
            while let Some((idx, ch)) = chars.next() {
                let hit = item.indices.contains(&(idx as u32));
                if hit != buf_hit && !buf.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut buf),
                        if buf_hit { theme.match_hit } else { theme.text },
                    ));
                }
                buf_hit = hit;
                buf.push(ch);
                if chars.peek().is_none() {
                    spans.push(Span::styled(
                        std::mem::take(&mut buf),
                        if buf_hit { theme.match_hit } else { theme.text },
                    ));
                }
            }

            if !item.candidate.detail.is_empty() {
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                let pad = width.saturating_sub(used + item.candidate.detail.chars().count() + 1);
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(item.candidate.detail.clone(), theme.dim));
            }

            let line = Line::from(spans);
            if selected {
                line.style(theme.selection)
            } else {
                line
            }
        })
        .collect()
}

fn draw_preview(
    f: &mut Frame,
    app: &mut App,
    theme: &Theme,
    area: Rect,
    target: Option<Target>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border);

    let Some(target) = target else {
        f.render_widget(
            Paragraph::new(Line::styled("  no results", theme.dim)).block(block),
            area,
        );
        return;
    };

    let preview = app.preview(&target);
    let block = block.title_top(Line::from(format!(" {} ", preview.title)).style(theme.title));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let height = inner.height as usize;
    let start = preview.focus.saturating_sub(height / 3);
    let start = start.min(preview.lines.len().saturating_sub(height.min(preview.lines.len())));

    let width = inner.width as usize;
    let lines: Vec<Line> = preview
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, text)| {
            let runs = vec![(text.as_str(), theme.context)];
            let mut spans = vec![Span::styled(format!("{:>5} ", i + 1), theme.gutter)];
            spans.extend(layout(runs, 0, width.saturating_sub(6)));
            let line = Line::from(spans);
            if i == preview.focus {
                line.style(theme.selection)
            } else {
                line
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------- help

fn draw_help(f: &mut Frame, app: &App, theme: &Theme, scroll: usize) {
    let area = centered(f.area(), 74, 84);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_focus)
        .title_top(Line::from(" keys ").style(theme.title))
        .title_bottom(
            Line::from(" any key closes ")
                .style(theme.dim)
                .alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (heading, ctx) in [("diff pane", Context::Diff), ("tree pane", Context::Tree)] {
        lines.push(Line::styled(format!("  {heading}"), theme.title));
        let mut entries: Vec<(String, String)> = app
            .keymap
            .entries(ctx)
            .map(|(k, a)| (k, describe(a).to_string()))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        for (key, what) in entries {
            lines.push(Line::from(vec![
                Span::styled(format!("    {key:<12}"), theme.prompt),
                Span::styled(what, theme.text),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled("  picker", theme.title));
    for (key, what) in PICKER_HELP {
        lines.push(Line::from(vec![
            Span::styled(format!("    {key:<12}"), theme.prompt),
            Span::styled(*what, theme.text),
        ]));
    }

    let max = lines.len().saturating_sub(inner.height as usize);
    let scroll = scroll.min(max);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll as u16, 0)),
        inner,
    );
}

const PICKER_HELP: &[(&str, &str)] = &[
    ("<C-j> <C-n>", "next result"),
    ("<C-k> <C-p>", "previous result"),
    ("<CR>", "open"),
    ("<C-q>", "send results to the quickfix list"),
    ("<C-u>", "clear the prompt"),
    ("<C-w>", "delete the previous word"),
    ("<Esc>", "close"),
];

fn describe(a: crate::keys::Action) -> &'static str {
    use crate::keys::Action as A;
    match a {
        A::Quit => "quit",
        A::Help => "this help",
        A::Refresh => "re-read the repository",
        A::ToggleTree => "toggle the file tree",
        A::FocusTree => "focus the tree",
        A::FocusDiff => "focus the diff",
        A::ToggleLayout => "toggle split / unified",
        A::WidenTree => "widen the tree",
        A::NarrowTree => "narrow the tree",
        A::Down => "down",
        A::Up => "up",
        A::HalfPageDown => "half page down",
        A::HalfPageUp => "half page up",
        A::PageDown => "page down",
        A::PageUp => "page up",
        A::Top => "first line",
        A::Bottom => "last line",
        A::Center => "centre the cursor",
        A::NextHunk => "next hunk",
        A::PrevHunk => "previous hunk",
        A::NextFile => "next changed file",
        A::PrevFile => "previous changed file",
        A::NextQuickfix => "next quickfix entry",
        A::PrevQuickfix => "previous quickfix entry",
        A::ScrollLeft => "scroll left",
        A::ScrollRight => "scroll right",
        A::ScrollHome => "scroll back to column 0",
        A::ToggleFullContext => "toggle whole-file view",
        A::ToggleGroupDirs => "toggle grouped directories",
        A::SearchPrompt => "search in this diff",
        A::SearchNext => "next match",
        A::SearchPrev => "previous match",
        A::PickFiles => "find changed file",
        A::PickGrep => "live grep the changes",
        A::PickGrepWord => "grep the word under the cursor",
        A::PickBuffers => "recently opened files",
        A::PickLines => "find a line in this diff",
        A::PickResume => "resume the last picker",
        A::TreeOpen => "open / expand",
        A::TreeToggleNode => "expand or collapse",
        A::TreeCloseNode => "collapse this directory",
        A::TreeCloseAll => "collapse everything",
        A::TreeExpandAll => "expand everything",
        A::TreeNavigateUp => "go to the parent directory",
        A::TreeFilter => "filter the tree",
        A::TreeClearFilter => "clear the tree filter",
        A::OpenEditor => "open in $EDITOR",
        A::YankPath => "copy the path",
        A::SwapSides => "swap the two sides",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn tabs_expand_to_the_next_tab_stop() {
        let runs = vec![("a\tb", Style::new())];
        assert_eq!(plain(&layout(runs, 0, 100)), "a   b");
    }

    #[test]
    fn horizontal_scroll_drops_leading_columns() {
        let runs = vec![("abcdefgh", Style::new())];
        assert_eq!(plain(&layout(runs, 3, 100)), "defgh");
    }

    #[test]
    fn output_is_clipped_to_the_pane_width() {
        let runs = vec![("abcdefgh", Style::new())];
        assert_eq!(plain(&layout(runs, 0, 3)), "abc");
    }

    #[test]
    fn control_characters_are_shown_as_dots() {
        let runs = vec![("a\rb", Style::new())];
        assert_eq!(plain(&layout(runs, 0, 100)), "a·b");
    }

    #[test]
    fn runs_split_at_mark_boundaries() {
        let base = Style::new();
        let mark = Style::new().add_modifier(Modifier::BOLD);
        let runs = split_runs("hello world", base, &[(6, 11, mark)]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, "hello ");
        assert_eq!(runs[1].0, "world");
        assert_eq!(runs[1].1, mark);
    }

    #[test]
    fn marks_inside_a_character_never_drop_text() {
        let base = Style::new();
        let mark = Style::new().add_modifier(Modifier::BOLD);
        // Byte 1 is inside the two-byte 'é'; the text must survive regardless.
        assert_eq!(plain_runs(&split_runs("é", base, &[(1, 2, mark)])), "é");
        assert_eq!(plain_runs(&split_runs("aéb", base, &[(1, 2, mark)])), "aéb");
    }

    #[test]
    fn wide_characters_count_two_columns() {
        let runs = vec![("日本語", Style::new())];
        // Three double-width glyphs cannot fit in five columns.
        assert_eq!(plain(&layout(runs, 0, 5)), "日本");
    }

    fn plain_runs(runs: &[(&str, Style)]) -> String {
        runs.iter().map(|(s, _)| *s).collect()
    }

    #[test]
    fn find_all_is_smart_case() {
        assert_eq!(find_all("Foo foo", "foo"), vec![(0, 3), (4, 7)]);
        assert_eq!(find_all("Foo foo", "Foo"), vec![(0, 3)]);
        assert!(find_all("abc", "").is_empty());
    }

    #[test]
    fn find_all_skips_non_ascii_folding() {
        // Folding would shift byte offsets, so no highlight is safer.
        assert!(find_all("İstanbul", "i").is_empty());
    }
}
