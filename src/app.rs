//! Application state and the action dispatcher.

use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::diff::{self, Body, Document};
use crate::git::{Blob, FileEntry, Repo, Spec};
use crate::keys::{self, Action, Context, Key, Keymap, Resolved};
use crate::picker::{Candidate, Kind, Picker, Target};
use crate::search;
use crate::tree::Tree;

/// Lines of unchanged context kept around each hunk, matching `git diff`.
const CONTEXT: usize = 3;
/// Rows kept between the cursor and the edge of the viewport (`scrolloff`).
const SCROLLOFF: usize = 4;
/// Columns moved per `h` / `l`.
const HSTEP: usize = 8;
/// Far enough right for any sane source line; keeps `l` from running away.
const MAX_HSCROLL: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Old on the left, new on the right.
    Split,
    /// One column, `-`/`+` prefixed.
    Unified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `/` in the diff pane.
    Search,
    /// `/` in the tree pane: neo-tree's fuzzy finder.
    TreeFilter,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
}

impl Prompt {
    pub fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::Search => "/",
            PromptKind::TreeFilter => "filter ",
        }
    }
}

pub enum Overlay {
    Picker(Picker),
    Prompt(Prompt),
    Help { scroll: usize },
}

/// Something the event loop must do outside the TUI.
pub enum Effect {
    None,
    /// Drop out of raw mode and run `$EDITOR`.
    OpenEditor { path: String, line: usize },
}

#[derive(Default)]
struct Find {
    query: String,
    matches: Vec<usize>,
    at: usize,
}

/// Enough to reopen a picker with `<leader>fr`.
struct LastPicker {
    kind: Kind,
    prompt: String,
}

pub struct App {
    pub repo: Repo,
    pub spec: Spec,
    pub files: Vec<FileEntry>,
    pub tree: Tree,

    /// Index into `files` of whatever the diff panes are showing.
    pub current: Option<usize>,
    pub doc: Option<Document>,

    pub cursor: usize,
    pub offset: usize,
    /// Columns scrolled off the left of each pane, for long lines.
    pub hscroll: usize,
    /// Rows the diff pane can show; refreshed by the renderer each frame.
    pub viewport: usize,

    pub focus: Context,
    /// Scroll offset of the tree pane; the renderer keeps it in range.
    pub tree_offset: usize,
    pub show_tree: bool,
    pub tree_width: u16,
    pub layout: Layout,
    /// `za`: show the whole file instead of hunks with context.
    pub full_context: bool,
    /// `s`: put the new side on the left.
    pub swapped: bool,

    pub keymap: Keymap,
    pub pending: Vec<Key>,
    pub overlay: Option<Overlay>,
    pub message: Option<String>,
    pub quit: bool,

    pub quickfix: Vec<Target>,
    pub quickfix_at: usize,

    /// Most-recently-viewed paths, newest first -- the `<leader>fb` list.
    recent: Vec<String>,
    last_picker: Option<LastPicker>,
    find: Find,
    /// New-side text per path, reused by the in-memory grep.
    blobs: HashMap<String, String>,
}

impl App {
    pub fn new(repo: Repo, spec: Spec) -> Result<Self> {
        let files = repo.changed_files(&spec)?;
        let tree = Tree::new(&files);
        let mut app = Self {
            repo,
            spec,
            files,
            tree,
            current: None,
            doc: None,
            cursor: 0,
            offset: 0,
            hscroll: 0,
            viewport: 1,
            focus: Context::Diff,
            tree_offset: 0,
            show_tree: true,
            tree_width: 34,
            layout: Layout::Split,
            full_context: false,
            swapped: false,
            keymap: Keymap::new(),
            pending: Vec::new(),
            overlay: None,
            message: None,
            quit: false,
            quickfix: Vec::new(),
            quickfix_at: 0,
            recent: Vec::new(),
            last_picker: None,
            find: Find::default(),
            blobs: HashMap::new(),
        };
        // Open the first changed file so the panes are never blank on launch.
        if let Some(i) = app.tree.step_file(true) {
            app.open_file(i);
        }
        Ok(app)
    }

    pub fn current_file(&self) -> Option<&FileEntry> {
        self.current.and_then(|i| self.files.get(i))
    }

    pub fn current_row(&self) -> Option<&diff::Row> {
        self.doc.as_ref()?.rows.get(self.cursor)
    }

    pub fn find_query(&self) -> &str {
        &self.find.query
    }

    pub fn find_matches(&self) -> &[usize] {
        &self.find.matches
    }

    // ---------------------------------------------------------------- loading

    fn open_file(&mut self, index: usize) {
        let Some(file) = self.files.get(index).cloned() else {
            return;
        };
        let old = self.repo.old_blob(&self.spec, &file);
        let new = self.repo.new_blob(&self.spec, &file);

        let (old, new) = match (old, new) {
            (Ok(o), Ok(n)) => (o, n),
            (Err(e), _) | (_, Err(e)) => {
                self.message = Some(format!("{e}"));
                (Blob::Absent, Blob::Absent)
            }
        };

        if let Blob::Text(t) = &new {
            self.blobs.insert(file.path.clone(), t.clone());
        }

        let doc = if matches!(old, Blob::Binary) || matches!(new, Blob::Binary) {
            Document::binary()
        } else {
            let context = if self.full_context { None } else { Some(CONTEXT) };
            let doc = diff::build(old.text(), new.text(), context);
            match self.layout {
                Layout::Split => doc,
                Layout::Unified => diff::unify(&doc),
            }
        };

        self.current = Some(index);
        self.doc = Some(doc);
        self.cursor = 0;
        self.offset = 0;
        self.hscroll = 0;
        self.refresh_find();
        self.touch_recent(&file.path);
        self.tree.reveal(&file.path);
    }

    fn touch_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(50);
    }

    /// Re-read the repository, keeping the cursor where it can be kept.
    pub fn reload(&mut self) {
        match self.repo.changed_files(&self.spec) {
            Ok(files) => {
                let path = self.current_file().map(|f| f.path.clone());
                self.files = files;
                self.blobs.clear();
                self.tree.reload(&self.files);
                self.current = None;
                self.doc = None;
                let index = path
                    .as_deref()
                    .and_then(|p| self.files.iter().position(|f| f.path == p));
                match index {
                    Some(i) => self.open_file(i),
                    None => {
                        if let Some(i) = self.tree.step_file(true) {
                            self.open_file(i);
                        }
                    }
                }
                self.message = Some(format!("reloaded -- {} changed files", self.files.len()));
            }
            Err(e) => self.message = Some(format!("{e}")),
        }
    }

    fn rebuild_doc(&mut self) {
        if let Some(i) = self.current {
            let keep = self.cursor;
            self.open_file(i);
            self.cursor = keep.min(self.doc.as_ref().map_or(0, |d| d.rows.len().saturating_sub(1)));
            self.scroll_into_view();
        }
    }

    // ------------------------------------------------------------ key routing

    pub fn on_key(&mut self, ev: KeyEvent) -> Effect {
        // Terminals that report key releases would otherwise fire twice.
        if ev.kind == KeyEventKind::Release {
            return Effect::None;
        }
        self.message = None;

        match &mut self.overlay {
            Some(Overlay::Picker(_)) => return self.picker_key(ev),
            Some(Overlay::Prompt(_)) => return self.prompt_key(ev),
            Some(Overlay::Help { .. }) => return self.help_key(ev),
            None => {}
        }

        let key = Key::from_event(ev);
        let ctx = self.focus;
        match keys::resolve(&self.keymap, ctx, &mut self.pending, key) {
            Resolved::Action(a) => self.act(a),
            Resolved::ActionThenReplay(a, replay) => {
                let effect = self.act(a);
                if !matches!(effect, Effect::None) {
                    return effect;
                }
                // The key that broke the sequence starts a fresh one.
                match keys::resolve(&self.keymap, self.focus, &mut self.pending, replay) {
                    Resolved::Action(a) => self.act(a),
                    Resolved::ActionThenReplay(a, _) => self.act(a),
                    _ => Effect::None,
                }
            }
            Resolved::Pending | Resolved::None => Effect::None,
        }
    }

    /// Nothing followed a partial sequence before the timeout expired, so
    /// settle for the longest complete binding it contains -- nvim's
    /// `timeoutlen` behaviour, which is what lets `<Space>` toggle a tree node.
    pub fn flush_pending(&mut self) -> Effect {
        let Some(action) = keys::longest_complete(&self.keymap, self.focus, &self.pending) else {
            self.pending.clear();
            return Effect::None;
        };
        self.pending.clear();
        self.act(action)
    }

    fn help_key(&mut self, ev: KeyEvent) -> Effect {
        let Some(Overlay::Help { scroll }) = &mut self.overlay else {
            return Effect::None;
        };
        match ev.code {
            KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char('g') => *scroll = 0,
            _ => self.overlay = None,
        }
        Effect::None
    }

    fn prompt_key(&mut self, ev: KeyEvent) -> Effect {
        let Some(Overlay::Prompt(p)) = &mut self.overlay else {
            return Effect::None;
        };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Esc | KeyCode::Char('c') if ctrl || ev.code == KeyCode::Esc => {
                let kind = p.kind;
                self.overlay = None;
                if kind == PromptKind::TreeFilter {
                    self.tree.set_filter("", &self.files);
                }
            }
            KeyCode::Enter => {
                let (kind, input) = (p.kind, p.input.clone());
                self.overlay = None;
                if kind == PromptKind::Search {
                    self.find.query = input;
                    self.refresh_find();
                    self.jump_to_match(true, true);
                }
            }
            KeyCode::Backspace => {
                p.input.pop();
                let (kind, input) = (p.kind, p.input.clone());
                if kind == PromptKind::TreeFilter {
                    self.tree.set_filter(&input, &self.files);
                }
            }
            KeyCode::Char('u') if ctrl => {
                p.input.clear();
                if p.kind == PromptKind::TreeFilter {
                    self.tree.set_filter("", &self.files);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                p.input.push(c);
                let (kind, input) = (p.kind, p.input.clone());
                if kind == PromptKind::TreeFilter {
                    self.tree.set_filter(&input, &self.files);
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn picker_key(&mut self, ev: KeyEvent) -> Effect {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Picker(p)) = &mut self.overlay else {
            return Effect::None;
        };

        match ev.code {
            KeyCode::Esc => {
                self.remember_picker();
                self.overlay = None;
            }
            KeyCode::Char('c') if ctrl => {
                self.remember_picker();
                self.overlay = None;
            }
            KeyCode::Enter => {
                let target = p.selected_target().cloned();
                self.remember_picker();
                self.overlay = None;
                if let Some(t) = target {
                    self.goto(&t);
                }
            }
            // Telescope's `<C-q>`: send the whole result set to the quickfix list.
            KeyCode::Char('q') if ctrl => {
                let targets = p.all_targets();
                self.remember_picker();
                self.overlay = None;
                if targets.is_empty() {
                    self.message = Some("no results to send".into());
                } else {
                    self.message = Some(format!("{} items in quickfix -- ]q / [q", targets.len()));
                    self.quickfix = targets;
                    self.quickfix_at = 0;
                    let first = self.quickfix[0].clone();
                    self.goto(&first);
                }
            }
            KeyCode::Char('j') | KeyCode::Char('n') if ctrl => p.move_selection(1),
            KeyCode::Char('k') | KeyCode::Char('p') if ctrl => p.move_selection(-1),
            KeyCode::Down => p.move_selection(1),
            KeyCode::Up => p.move_selection(-1),
            KeyCode::Char('u') if ctrl => p.clear_prompt(),
            KeyCode::Char('w') if ctrl => {
                p.delete_word();
                self.refresh_live_picker();
            }
            KeyCode::Backspace => {
                p.backspace();
                self.refresh_live_picker();
            }
            KeyCode::Char(c) if !ctrl => {
                p.insert(c);
                self.refresh_live_picker();
            }
            _ => {}
        }
        Effect::None
    }

    fn remember_picker(&mut self) {
        if let Some(Overlay::Picker(p)) = &self.overlay {
            self.last_picker = Some(LastPicker {
                kind: p.kind,
                prompt: p.prompt.clone(),
            });
        }
    }

    // --------------------------------------------------------------- actions

    fn act(&mut self, action: Action) -> Effect {
        match action {
            Action::Quit => self.quit = true,
            Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Action::Refresh => self.reload(),

            Action::ToggleTree => {
                self.show_tree = !self.show_tree;
                self.focus = if self.show_tree {
                    Context::Tree
                } else {
                    Context::Diff
                };
            }
            Action::FocusTree => {
                if self.show_tree {
                    self.focus = Context::Tree;
                }
            }
            Action::FocusDiff => self.focus = Context::Diff,
            Action::ToggleLayout => {
                self.layout = match self.layout {
                    Layout::Split => Layout::Unified,
                    Layout::Unified => Layout::Split,
                };
                // The row list itself differs between the two layouts.
                self.rebuild_doc();
            }
            Action::WidenTree => self.tree_width = (self.tree_width + 4).min(90),
            Action::NarrowTree => self.tree_width = self.tree_width.saturating_sub(4).max(14),

            Action::Down => self.move_cursor(1),
            Action::Up => self.move_cursor(-1),
            Action::HalfPageDown => self.move_cursor(self.viewport as isize / 2),
            Action::HalfPageUp => self.move_cursor(-(self.viewport as isize / 2)),
            Action::PageDown => self.move_cursor(self.viewport as isize),
            Action::PageUp => self.move_cursor(-(self.viewport as isize)),
            Action::Top => self.goto_row(0),
            Action::Bottom => {
                let last = match self.focus {
                    Context::Tree => self.tree.nodes.len(),
                    Context::Diff => self.doc.as_ref().map_or(0, |d| d.rows.len()),
                };
                self.goto_row(last.saturating_sub(1));
            }
            Action::Center => {
                self.offset = self.cursor.saturating_sub(self.viewport / 2);
            }

            Action::NextHunk => self.step_hunk(true),
            Action::PrevHunk => self.step_hunk(false),
            Action::NextFile => self.step_file(true),
            Action::PrevFile => self.step_file(false),
            Action::NextQuickfix => self.step_quickfix(1),
            Action::PrevQuickfix => self.step_quickfix(-1),

            Action::ScrollLeft => self.hscroll = self.hscroll.saturating_sub(HSTEP),
            Action::ScrollRight => self.hscroll = (self.hscroll + HSTEP).min(MAX_HSCROLL),
            Action::ScrollHome => self.hscroll = 0,

            Action::ToggleFullContext => {
                self.full_context = !self.full_context;
                self.rebuild_doc();
                self.message = Some(if self.full_context {
                    "showing whole file".into()
                } else {
                    format!("showing {CONTEXT} lines of context")
                });
            }
            Action::ToggleGroupDirs => {
                self.tree.toggle_group_dirs(&self.files);
            }
            Action::SwapSides => {
                self.swapped = !self.swapped;
            }

            Action::SearchPrompt => {
                self.overlay = Some(Overlay::Prompt(Prompt {
                    kind: PromptKind::Search,
                    input: String::new(),
                }))
            }
            Action::SearchNext => self.jump_to_match(true, false),
            Action::SearchPrev => self.jump_to_match(false, false),

            Action::PickFiles => self.open_picker(Kind::Files, None),
            Action::PickBuffers => self.open_picker(Kind::Buffers, None),
            Action::PickLines => self.open_picker(Kind::Lines, None),
            Action::PickGrep => self.open_picker(Kind::Grep, None),
            Action::PickGrepWord => {
                let word = self.word_under_cursor();
                if word.is_empty() {
                    self.message = Some("no word under the cursor".into());
                } else {
                    self.open_picker(Kind::Grep, Some(word));
                }
            }
            Action::PickResume => match self.last_picker.take() {
                Some(last) => {
                    let prompt = last.prompt.clone();
                    self.open_picker(last.kind, Some(prompt));
                }
                None => self.message = Some("no picker to resume".into()),
            },

            Action::TreeOpen => {
                if self.tree.selected_node().is_some_and(|n| n.is_dir()) {
                    self.tree.toggle(&self.files);
                } else if let Some(i) = self.tree.selected_file() {
                    self.open_file(i);
                    self.focus = Context::Diff;
                }
            }
            Action::TreeToggleNode => self.tree.toggle(&self.files),
            Action::TreeCloseNode => self.tree.close_node(&self.files),
            Action::TreeCloseAll => self.tree.close_all(&self.files),
            Action::TreeExpandAll => self.tree.expand_all(&self.files),
            Action::TreeNavigateUp => self.tree.navigate_up(),
            Action::TreeFilter => {
                self.overlay = Some(Overlay::Prompt(Prompt {
                    kind: PromptKind::TreeFilter,
                    input: self.tree.filter().to_string(),
                }))
            }
            Action::TreeClearFilter => self.tree.set_filter("", &self.files),

            Action::YankPath => {
                if let Some(f) = self.current_file() {
                    let path = f.path.clone();
                    crate::clipboard::copy(&path);
                    self.message = Some(format!("yanked {path}"));
                }
            }
            Action::OpenEditor => {
                if self.focus == Context::Tree {
                    if self.tree.selected_node().is_some_and(|n| n.is_dir()) {
                        self.tree.toggle(&self.files);
                        return Effect::None;
                    }
                    if let Some(i) = self.tree.selected_file() {
                        self.open_file(i);
                        self.focus = Context::Diff;
                        return Effect::None;
                    }
                }
                if let Some(file) = self.current_file() {
                    let path = file.path.clone();
                    let line = self.current_row().map_or(1, |r| r.editor_line());
                    return Effect::OpenEditor { path, line };
                }
            }
        }
        Effect::None
    }

    // -------------------------------------------------------------- movement

    fn rows_len(&self) -> usize {
        match self.focus {
            Context::Tree => self.tree.nodes.len(),
            Context::Diff => self.doc.as_ref().map_or(0, |d| d.rows.len()),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.focus {
            Context::Tree => self.tree.move_by(delta),
            Context::Diff => {
                let len = self.rows_len();
                if len == 0 {
                    return;
                }
                let next = (self.cursor as isize + delta).clamp(0, len as isize - 1);
                self.cursor = next as usize;
                self.scroll_into_view();
            }
        }
    }

    fn goto_row(&mut self, row: usize) {
        match self.focus {
            Context::Tree => {
                if row == 0 {
                    self.tree.select_first()
                } else {
                    self.tree.select_last()
                }
            }
            Context::Diff => {
                self.cursor = row.min(self.rows_len().saturating_sub(1));
                self.scroll_into_view();
            }
        }
    }

    pub fn scroll_into_view(&mut self) {
        let len = self.doc.as_ref().map_or(0, |d| d.rows.len());
        let height = self.viewport.max(1);
        // With a short viewport, scrolloff would pin the cursor mid-screen.
        let pad = SCROLLOFF.min(height.saturating_sub(1) / 2);

        if self.cursor < self.offset + pad {
            self.offset = self.cursor.saturating_sub(pad);
        } else if self.cursor + pad >= self.offset + height {
            self.offset = (self.cursor + pad + 1).saturating_sub(height);
        }
        let max_offset = len.saturating_sub(height);
        self.offset = self.offset.min(max_offset);
    }

    fn step_hunk(&mut self, forward: bool) {
        let Some(doc) = &self.doc else { return };
        if doc.hunks.is_empty() {
            self.message = Some("no hunks".into());
            return;
        }
        let target = if forward {
            doc.hunks.iter().find(|&&r| r > self.cursor).copied()
        } else {
            doc.hunks.iter().rev().find(|&&r| r < self.cursor).copied()
        };
        match target {
            Some(row) => {
                self.cursor = row;
                self.scroll_into_view();
            }
            None => {
                // Wrap, like `]c` does at the end of a diff.
                let row = if forward {
                    doc.hunks[0]
                } else {
                    *doc.hunks.last().unwrap()
                };
                self.cursor = row;
                self.scroll_into_view();
                self.message = Some("search hit the end, wrapped".into());
            }
        }
    }

    fn step_file(&mut self, forward: bool) {
        match self.tree.step_file(forward) {
            Some(i) => {
                self.open_file(i);
            }
            None => self.message = Some("no changed files".into()),
        }
    }

    fn step_quickfix(&mut self, delta: isize) {
        if self.quickfix.is_empty() {
            self.message = Some("quickfix list is empty".into());
            return;
        }
        let len = self.quickfix.len() as isize;
        self.quickfix_at = (self.quickfix_at as isize + delta).rem_euclid(len) as usize;
        let target = self.quickfix[self.quickfix_at].clone();
        self.message = Some(format!("({}/{})", self.quickfix_at + 1, self.quickfix.len()));
        self.goto(&target);
    }

    // ---------------------------------------------------------------- search

    fn refresh_find(&mut self) {
        self.find.matches.clear();
        self.find.at = 0;
        if self.find.query.is_empty() {
            return;
        }
        let fold = !self.find.query.chars().any(char::is_uppercase);
        let needle = if fold {
            self.find.query.to_lowercase()
        } else {
            self.find.query.clone()
        };
        let Some(doc) = &self.doc else { return };
        for (i, row) in doc.rows.iter().enumerate() {
            let hay = if fold {
                row.search_text().to_lowercase()
            } else {
                row.search_text().to_string()
            };
            if hay.contains(&needle) {
                self.find.matches.push(i);
            }
        }
    }

    fn jump_to_match(&mut self, forward: bool, from_cursor: bool) {
        if self.find.matches.is_empty() {
            if !self.find.query.is_empty() {
                self.message = Some(format!("pattern not found: {}", self.find.query));
            }
            return;
        }
        let from = if from_cursor {
            self.cursor.saturating_sub(1)
        } else {
            self.cursor
        };
        let next = if forward {
            self.find
                .matches
                .iter()
                .position(|&r| r > from)
                .unwrap_or(0)
        } else {
            self.find
                .matches
                .iter()
                .rposition(|&r| r < self.cursor)
                .unwrap_or(self.find.matches.len() - 1)
        };
        self.find.at = next;
        self.cursor = self.find.matches[next];
        self.scroll_into_view();
        self.message = Some(format!(
            "match {}/{}",
            self.find.at + 1,
            self.find.matches.len()
        ));
    }

    /// The identifier the cursor is sitting on, for `<leader>fw`.
    pub fn word_under_cursor(&self) -> String {
        let Some(row) = self.current_row() else {
            return String::new();
        };
        // On a changed row the interesting word is the one that changed.
        let cell = row.new.as_ref().or(row.old.as_ref());
        if let Some(cell) = cell {
            if let Some(&(s, e)) = cell.emphasis.first() {
                let piece = cell.text.get(s..e).unwrap_or("").trim();
                if piece.chars().any(is_word) {
                    return piece
                        .split(|c: char| !is_word(c))
                        .find(|t| !t.is_empty())
                        .unwrap_or(piece)
                        .to_string();
                }
            }
        }
        // Otherwise the longest identifier on the line is the best guess.
        row.search_text()
            .split(|c: char| !is_word(c))
            .filter(|t| !t.is_empty())
            .max_by_key(|t| t.len())
            .unwrap_or("")
            .to_string()
    }

    // --------------------------------------------------------------- pickers

    fn open_picker(&mut self, kind: Kind, prompt: Option<String>) {
        let mut picker = match kind {
            Kind::Files => Picker::new(kind, self.files_title(), self.file_candidates()),
            Kind::Buffers => {
                let source = self.buffer_candidates();
                if source.is_empty() {
                    self.message = Some("no files opened yet".into());
                    return;
                }
                Picker::new(kind, "Recent", source)
            }
            Kind::Lines => {
                let source = self.line_candidates();
                if source.is_empty() {
                    self.message = Some("nothing to search in this file".into());
                    return;
                }
                Picker::new(kind, "Lines in diff", source)
            }
            Kind::Grep => Picker::new(kind, self.grep_title(), Vec::new()),
        };
        if let Some(p) = prompt {
            picker = picker.with_prompt(p);
        }
        self.overlay = Some(Overlay::Picker(picker));
        self.refresh_live_picker();
    }

    fn files_title(&self) -> String {
        format!("Changed files ({})", self.files.len())
    }

    fn grep_title(&self) -> String {
        if search::uses_ripgrep(&self.spec) {
            "Live grep (ripgrep, changed files)".into()
        } else {
            "Live grep (in diff contents)".into()
        }
    }

    fn file_candidates(&self) -> Vec<Candidate> {
        self.files
            .iter()
            .map(|f| Candidate {
                display: f.path.clone(),
                detail: f.status.badge().to_string(),
                target: Target::File(f.path.clone()),
            })
            .collect()
    }

    fn buffer_candidates(&self) -> Vec<Candidate> {
        self.recent
            .iter()
            .map(|p| Candidate {
                display: p.clone(),
                detail: String::new(),
                target: Target::File(p.clone()),
            })
            .collect()
    }

    fn line_candidates(&self) -> Vec<Candidate> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        doc.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.search_text().trim().is_empty())
            .map(|(i, r)| Candidate {
                display: r.search_text().trim_end().to_string(),
                detail: format!("{}", r.editor_line()),
                target: Target::Row(i),
            })
            .collect()
    }

    /// Re-run the grep backend after the prompt changed.
    fn refresh_live_picker(&mut self) {
        let Some(Overlay::Picker(p)) = &self.overlay else {
            return;
        };
        if !p.kind.is_live() {
            return;
        }
        let query = p.prompt.clone();
        // One character matches nearly everything; wait for a real query.
        let hits = if query.chars().count() < 2 {
            Vec::new()
        } else if search::uses_ripgrep(&self.spec) {
            let paths: Vec<String> = self.files.iter().map(|f| f.path.clone()).collect();
            search::ripgrep(self.repo.root(), &query, &paths)
        } else {
            let sources = self.grep_sources();
            search::in_memory(&query, &sources)
        };

        let candidates = hits
            .into_iter()
            .map(|h| Candidate {
                display: h.text.clone(),
                detail: format!("{}:{}", h.path, h.line),
                target: Target::Location {
                    path: h.path,
                    line: h.line,
                },
            })
            .collect();

        if let Some(Overlay::Picker(p)) = &mut self.overlay {
            p.set_items(candidates);
        }
    }

    /// New-side contents of every changed file, fetched on demand.
    fn grep_sources(&mut self) -> Vec<(String, String)> {
        let files = self.files.clone();
        for f in &files {
            if self.blobs.contains_key(&f.path) {
                continue;
            }
            if let Ok(Blob::Text(t)) = self.repo.new_blob(&self.spec, f) {
                self.blobs.insert(f.path.clone(), t);
            }
        }
        files
            .iter()
            .filter_map(|f| self.blobs.get(&f.path).map(|t| (f.path.clone(), t.clone())))
            .collect()
    }

    /// Act on a picker or quickfix selection.
    fn goto(&mut self, target: &Target) {
        match target {
            Target::File(path) => {
                if let Some(i) = self.files.iter().position(|f| &f.path == path) {
                    self.open_file(i);
                    self.focus = Context::Diff;
                } else {
                    self.message = Some(format!("{path} is no longer in the diff"));
                }
            }
            Target::Location { path, line } => {
                let Some(i) = self.files.iter().position(|f| &f.path == path) else {
                    self.message = Some(format!("{path} is no longer in the diff"));
                    return;
                };
                if self.current != Some(i) {
                    self.open_file(i);
                }
                self.focus = Context::Diff;
                self.goto_new_line(*line);
            }
            Target::Row(row) => {
                self.focus = Context::Diff;
                self.cursor = (*row).min(self.rows_len().saturating_sub(1));
                self.scroll_into_view();
            }
        }
    }

    /// Put the cursor on the row holding line `line` of the new side.
    ///
    /// With context folded the line may not be on screen at all; say so rather
    /// than silently landing somewhere else.
    fn goto_new_line(&mut self, line: usize) {
        let Some(doc) = &self.doc else { return };
        let exact = doc
            .rows
            .iter()
            .position(|r| r.new.as_ref().is_some_and(|c| c.number == line));
        match exact {
            Some(row) => {
                self.cursor = row;
                self.scroll_into_view();
            }
            None => {
                // Land on the nearest row that precedes it, so the neighbourhood
                // is at least right.
                let near = doc
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.new.as_ref().is_some_and(|c| c.number < line))
                    .next_back()
                    .map(|(i, _)| i);
                self.cursor = near.unwrap_or(0);
                self.scroll_into_view();
                self.message = Some(format!(
                    "line {line} is outside the diff -- press za to show the whole file"
                ));
            }
        }
    }

    /// Lines shown in the picker's preview pane, plus the one to centre on.
    ///
    /// Blobs are cached, so moving up and down a result list costs at most one
    /// `git show` per distinct file.
    pub fn preview(&mut self, target: &Target) -> Preview {
        match target {
            Target::Row(row) => {
                let Some(doc) = &self.doc else {
                    return Preview::default();
                };
                let title = self
                    .current_file()
                    .map_or_else(String::new, |f| f.path.clone());
                let lines = doc
                    .rows
                    .iter()
                    .map(|r| r.search_text().to_string())
                    .collect();
                Preview {
                    title,
                    lines,
                    focus: *row,
                }
            }
            Target::File(path) => {
                let lines = self.preview_blob(path);
                Preview {
                    title: path.clone(),
                    lines,
                    focus: 0,
                }
            }
            Target::Location { path, line } => {
                let lines = self.preview_blob(path);
                Preview {
                    title: format!("{path}:{line}"),
                    lines,
                    focus: line.saturating_sub(1),
                }
            }
        }
    }

    fn preview_blob(&mut self, path: &str) -> Vec<String> {
        if !self.blobs.contains_key(path) {
            let Some(file) = self.files.iter().find(|f| f.path == path).cloned() else {
                return Vec::new();
            };
            // Deleted files have no new side; fall back to the old one so the
            // preview is not blank.
            let blob = self
                .repo
                .new_blob(&self.spec, &file)
                .ok()
                .filter(|b| matches!(b, Blob::Text(_)))
                .or_else(|| self.repo.old_blob(&self.spec, &file).ok());
            match blob {
                Some(Blob::Text(t)) => {
                    self.blobs.insert(path.to_string(), t);
                }
                Some(Blob::Binary) => return vec!["<binary file>".into()],
                _ => return Vec::new(),
            }
        }
        self.blobs
            .get(path)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Which hunk the cursor is in, as `(nth, total)`.
    pub fn hunk_position(&self) -> Option<(usize, usize)> {
        let doc = self.doc.as_ref()?;
        if doc.hunks.is_empty() {
            return None;
        }
        let row = doc.rows.get(self.cursor)?;
        Some((row.hunk.max(1), doc.hunks.len()))
    }

    /// Status-line summary of the current file.
    pub fn summary(&self) -> String {
        let Some(doc) = &self.doc else {
            return "no file".into();
        };
        match doc.body {
            Body::Binary => "binary".into(),
            Body::Identical => "no content change".into(),
            Body::Text => format!("+{} -{}", doc.added, doc.removed),
        }
    }

    pub fn pending_label(&self) -> String {
        keys::render_pending(&self.pending)
    }
}

/// Contents of the picker's preview pane.
#[derive(Default)]
pub struct Preview {
    pub title: String,
    pub lines: Vec<String>,
    /// Index of the line the preview should centre on.
    pub focus: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
