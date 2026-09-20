//! Turns two versions of a file into rows that the two panes share.
//!
//! The key invariant: the left and right panes render the *same* row range, so
//! a single scroll offset keeps them aligned. Rows where only one side exists
//! carry a `None` on the other side and render as filler.

use similar::{ChangeTag, DiffOp, TextDiff};

/// A single displayable line on one side.
#[derive(Debug, Clone)]
pub struct Cell {
    /// 1-based line number in that side's file.
    pub number: usize,
    pub text: String,
    /// Byte ranges within `text` that differ from the other side, used for
    /// word-level highlighting. Empty when the whole line is new or gone.
    pub emphasis: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Identical on both sides.
    Context,
    /// Present on both sides but modified.
    Changed,
    /// Only on the left.
    Removed,
    /// Only on the right.
    Added,
    /// The `@@ ... @@` gap between hunks.
    Separator,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub old: Option<Cell>,
    pub new: Option<Cell>,
    /// Index into [`Document::hunks`] that this row belongs to.
    pub hunk: usize,
}

impl Row {
    /// Text used by in-file search and the line picker: whichever side carries
    /// the meaning of this row.
    pub fn search_text(&self) -> &str {
        match (&self.new, &self.old) {
            (Some(c), _) => &c.text,
            (None, Some(c)) => &c.text,
            (None, None) => "",
        }
    }

    /// Line number to jump to when opening this row in an editor.
    pub fn editor_line(&self) -> usize {
        self.new
            .as_ref()
            .or(self.old.as_ref())
            .map_or(1, |c| c.number)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    Text,
    Binary,
    /// Both sides are byte-identical -- can happen for mode-only changes.
    Identical,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub rows: Vec<Row>,
    /// Row index where each hunk starts, for `]c` / `[c`.
    pub hunks: Vec<usize>,
    pub added: usize,
    pub removed: usize,
    pub body: Body,
}

impl Document {
    /// The hunk start to jump to from row `from`.
    ///
    /// Returns the target row and whether the search wrapped past an end.
    /// `None` when the file has no hunks at all.
    pub fn step_hunk(&self, from: usize, forward: bool) -> Option<(usize, bool)> {
        if self.hunks.is_empty() {
            return None;
        }
        let next = if forward {
            self.hunks.iter().find(|&&r| r > from).copied()
        } else {
            self.hunks.iter().rev().find(|&&r| r < from).copied()
        };
        match next {
            Some(row) => Some((row, false)),
            // Wrap, the way `]c` does at the end of a diff.
            None if forward => Some((self.hunks[0], true)),
            None => Some((*self.hunks.last().unwrap(), true)),
        }
    }

    pub fn binary() -> Self {
        Self {
            rows: Vec::new(),
            hunks: Vec::new(),
            added: 0,
            removed: 0,
            body: Body::Binary,
        }
    }
}

/// Lines below this similarity get no word-level highlight: a wholly rewritten
/// line produces confetti rather than a useful signal.
const WORD_HIGHLIGHT_FLOOR: f32 = 0.3;

/// Build the row list. `context` is the number of unchanged lines kept around
/// each hunk; `None` shows the entire file.
pub fn build(old: &str, new: &str, context: Option<usize>) -> Document {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    if old_lines == new_lines {
        // Still render the file so the panes aren't blank.
        let rows = old_lines
            .iter()
            .enumerate()
            .map(|(i, text)| Row {
                kind: RowKind::Context,
                old: Some(Cell::plain(i + 1, text)),
                new: Some(Cell::plain(i + 1, text)),
                hunk: 0,
            })
            .collect();
        return Document {
            rows,
            hunks: Vec::new(),
            added: 0,
            removed: 0,
            body: Body::Identical,
        };
    }

    let diff = TextDiff::from_slices(&old_lines, &new_lines);

    let groups: Vec<Vec<DiffOp>> = match context {
        Some(n) => diff.grouped_ops(n),
        None => vec![diff.ops().to_vec()],
    };

    let mut b = Builder::default();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            b.separator();
        }
        b.start_hunk();
        for op in group {
            b.op(op, &old_lines, &new_lines);
        }
    }

    Document {
        rows: b.rows,
        hunks: b.hunks,
        added: b.added,
        removed: b.removed,
        body: Body::Text,
    }
}

/// Split every `Changed` row into a removed row and an added row.
///
/// Side-by-side shows both versions of a modified line at once; unified has
/// only one column, so the pair has to become two rows. Doing it here keeps
/// one row equal to one screen line, which is what the cursor and the scroll
/// offset assume.
pub fn unify(doc: &Document) -> Document {
    let mut rows = Vec::with_capacity(doc.rows.len());
    // Old row index -> new row index, to move the hunk markers along with it.
    let mut remap = Vec::with_capacity(doc.rows.len());

    for row in &doc.rows {
        remap.push(rows.len());
        if row.kind == RowKind::Changed {
            rows.push(Row {
                kind: RowKind::Removed,
                old: row.old.clone(),
                new: None,
                hunk: row.hunk,
            });
            rows.push(Row {
                kind: RowKind::Added,
                old: None,
                new: row.new.clone(),
                hunk: row.hunk,
            });
        } else {
            rows.push(row.clone());
        }
    }

    Document {
        rows,
        hunks: doc.hunks.iter().map(|&i| remap[i]).collect(),
        added: doc.added,
        removed: doc.removed,
        body: doc.body,
    }
}

impl Cell {
    fn plain(number: usize, text: &str) -> Self {
        Self {
            number,
            text: text.to_string(),
            emphasis: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Builder {
    rows: Vec<Row>,
    hunks: Vec<usize>,
    added: usize,
    removed: usize,
}

impl Builder {
    /// Hunks are numbered from 1 so that row.hunk == 0 means "before any hunk".
    fn current_hunk(&self) -> usize {
        self.hunks.len()
    }

    fn start_hunk(&mut self) {
        self.hunks.push(self.rows.len());
    }

    fn separator(&mut self) {
        self.rows.push(Row {
            kind: RowKind::Separator,
            old: None,
            new: None,
            hunk: self.current_hunk(),
        });
    }

    fn push(&mut self, kind: RowKind, old: Option<Cell>, new: Option<Cell>) {
        match kind {
            RowKind::Added => self.added += 1,
            RowKind::Removed => self.removed += 1,
            RowKind::Changed => {
                self.added += 1;
                self.removed += 1;
            }
            _ => {}
        }
        let hunk = self.current_hunk();
        self.rows.push(Row {
            kind,
            old,
            new,
            hunk,
        });
    }

    fn op(&mut self, op: &DiffOp, old_lines: &[&str], new_lines: &[&str]) {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    self.push(
                        RowKind::Context,
                        Some(Cell::plain(old_index + k + 1, old_lines[old_index + k])),
                        Some(Cell::plain(new_index + k + 1, new_lines[new_index + k])),
                    );
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    self.push(
                        RowKind::Removed,
                        Some(Cell::plain(old_index + k + 1, old_lines[old_index + k])),
                        None,
                    );
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    self.push(
                        RowKind::Added,
                        None,
                        Some(Cell::plain(new_index + k + 1, new_lines[new_index + k])),
                    );
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // Pair lines positionally, then spill the remainder of the
                // longer side into one-sided rows.
                let paired = old_len.min(new_len);
                for k in 0..paired {
                    let o = old_lines[old_index + k];
                    let n = new_lines[new_index + k];
                    let (oe, ne) = word_emphasis(o, n);
                    self.push(
                        RowKind::Changed,
                        Some(Cell {
                            number: old_index + k + 1,
                            text: o.to_string(),
                            emphasis: oe,
                        }),
                        Some(Cell {
                            number: new_index + k + 1,
                            text: n.to_string(),
                            emphasis: ne,
                        }),
                    );
                }
                for k in paired..old_len {
                    self.push(
                        RowKind::Removed,
                        Some(Cell::plain(old_index + k + 1, old_lines[old_index + k])),
                        None,
                    );
                }
                for k in paired..new_len {
                    self.push(
                        RowKind::Added,
                        None,
                        Some(Cell::plain(new_index + k + 1, new_lines[new_index + k])),
                    );
                }
            }
        }
    }
}

/// Split into lines *without* terminators, keeping any `\r` so that a line
/// ending change still registers as a change.
fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = s.split('\n').collect();
    // A trailing newline yields a final empty element that is not a real line.
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}

/// Byte ranges that changed within a pair of lines, one list per side.
///
/// Returns empty lists when the lines are too dissimilar for the highlight to
/// mean anything.
fn word_emphasis(old: &str, new: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let diff = TextDiff::from_words(old, new);

    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let (mut old_off, mut new_off) = (0usize, 0usize);
    let mut equal_bytes = 0usize;

    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                equal_bytes += len;
                old_off += len;
                new_off += len;
            }
            ChangeTag::Delete => {
                old_ranges.push((old_off, old_off + len));
                old_off += len;
            }
            ChangeTag::Insert => {
                new_ranges.push((new_off, new_off + len));
                new_off += len;
            }
        }
    }

    let total = old.len() + new.len();
    let ratio = if total == 0 {
        1.0
    } else {
        (2 * equal_bytes) as f32 / total as f32
    };
    if ratio < WORD_HIGHLIGHT_FLOOR {
        return (Vec::new(), Vec::new());
    }

    (merge(old_ranges), merge(new_ranges))
}

/// Collapse touching ranges so highlighting doesn't flicker between adjacent
/// tokens that were both replaced.
fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.len() < 2 {
        return ranges;
    }
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sides_stay_aligned_row_for_row() {
        let doc = build("a\nb\nc\n", "a\nB\nc\n", None);
        assert_eq!(doc.rows.len(), 3);
        for row in &doc.rows {
            // No row may be blank on both sides outside a separator.
            assert!(row.old.is_some() || row.new.is_some());
        }
        assert_eq!(doc.rows[1].kind, RowKind::Changed);
        assert_eq!(doc.added, 1);
        assert_eq!(doc.removed, 1);
    }

    #[test]
    fn deletions_leave_the_right_side_empty() {
        let doc = build("a\nb\nc\n", "a\nc\n", None);
        let removed: Vec<_> = doc
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Removed)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].old.as_ref().unwrap().text, "b");
        assert!(removed[0].new.is_none());
    }

    #[test]
    fn line_numbers_track_each_side_independently() {
        let doc = build("a\nb\nc\n", "a\nx\ny\nb\nc\n", None);
        let last = doc.rows.last().unwrap();
        assert_eq!(last.old.as_ref().unwrap().number, 3);
        assert_eq!(last.new.as_ref().unwrap().number, 5);
    }

    #[test]
    fn word_emphasis_marks_only_the_changed_token() {
        let (old, new) = word_emphasis("let x = 1;", "let x = 2;");
        assert_eq!(old.len(), 1);
        assert_eq!(new.len(), 1);
        // The tokenizer keeps trailing punctuation with its word, so the span
        // is the whole token -- but it must not reach back over "let x".
        let (s, e) = new[0];
        assert!("let x = 2;"[s..e].contains('2'));
        assert!(!"let x = 2;"[s..e].contains('x'));
        assert!(s >= "let x = ".len());
    }

    #[test]
    fn wholly_rewritten_lines_get_no_confetti() {
        let (old, new) = word_emphasis("alpha beta gamma", "zzz qqq www");
        assert!(old.is_empty() && new.is_empty());
    }

    #[test]
    fn context_mode_inserts_separators_between_hunks() {
        let mut o = String::new();
        let mut n = String::new();
        for i in 0..100 {
            o.push_str(&format!("line {i}\n"));
            n.push_str(&format!("line {i}\n"));
        }
        let o = o.replace("line 5\n", "LINE 5\n");
        let n = n.replace("line 90\n", "LINE 90\n");
        let doc = build(&o, &n, Some(3));
        assert_eq!(doc.hunks.len(), 2);
        assert!(doc.rows.iter().any(|r| r.kind == RowKind::Separator));
        // Far fewer rows than the 100-line file: context is actually collapsed.
        assert!(doc.rows.len() < 30, "got {} rows", doc.rows.len());
    }

    #[test]
    fn unify_splits_changed_rows_in_two() {
        let doc = build("a\nb\nc\n", "a\nB\nc\n", None);
        let u = unify(&doc);
        assert_eq!(u.rows.len(), 4);
        assert_eq!(u.rows[1].kind, RowKind::Removed);
        assert_eq!(u.rows[1].old.as_ref().unwrap().text, "b");
        assert!(u.rows[1].new.is_none());
        assert_eq!(u.rows[2].kind, RowKind::Added);
        assert_eq!(u.rows[2].new.as_ref().unwrap().text, "B");
        // Counts are a property of the diff, not of how it is drawn.
        assert_eq!((u.added, u.removed), (doc.added, doc.removed));
    }

    #[test]
    fn unify_keeps_hunk_markers_pointing_at_the_right_rows() {
        let mut o = String::new();
        let mut n = String::new();
        for i in 0..60 {
            o.push_str(&format!("line {i}\n"));
            n.push_str(&format!("line {i}\n"));
        }
        let o = o.replace("line 5\n", "LINE 5\n");
        let n = n.replace("line 50\n", "LINE 50\n");
        let doc = build(&o, &n, Some(3));
        let u = unify(&doc);
        assert_eq!(u.hunks.len(), doc.hunks.len());
        for &row in &u.hunks {
            assert!(row < u.rows.len());
        }
        // Every hunk marker still lands on the first row of its hunk.
        for (i, &row) in u.hunks.iter().enumerate() {
            assert_eq!(u.rows[row].hunk, i + 1);
        }
    }

    /// A document with hunks starting at the given rows.
    fn with_hunks(hunks: &[usize]) -> Document {
        Document {
            rows: Vec::new(),
            hunks: hunks.to_vec(),
            added: 0,
            removed: 0,
            body: Body::Text,
        }
    }

    #[test]
    fn step_hunk_walks_forward_and_back() {
        let doc = with_hunks(&[0, 10, 20]);
        assert_eq!(doc.step_hunk(0, true), Some((10, false)));
        assert_eq!(doc.step_hunk(10, true), Some((20, false)));
        assert_eq!(doc.step_hunk(20, false), Some((10, false)));
        // From between two hunks, not just from a hunk start.
        assert_eq!(doc.step_hunk(5, true), Some((10, false)));
        assert_eq!(doc.step_hunk(15, false), Some((10, false)));
    }

    #[test]
    fn step_hunk_wraps_at_both_ends() {
        let doc = with_hunks(&[0, 10, 20]);
        assert_eq!(doc.step_hunk(20, true), Some((0, true)));
        assert_eq!(doc.step_hunk(0, false), Some((20, true)));
    }

    #[test]
    fn step_hunk_reports_a_file_with_no_hunks() {
        assert_eq!(with_hunks(&[]).step_hunk(0, true), None);
    }

    #[test]
    fn step_hunk_handles_a_single_hunk() {
        let doc = with_hunks(&[7]);
        // The only hunk is always the answer, and going there is a wrap.
        assert_eq!(doc.step_hunk(7, true), Some((7, true)));
        assert_eq!(doc.step_hunk(7, false), Some((7, true)));
        assert_eq!(doc.step_hunk(0, true), Some((7, false)));
    }

    #[test]
    fn empty_and_missing_sides_are_handled() {
        let doc = build("", "a\nb\n", None);
        assert_eq!(doc.added, 2);
        assert_eq!(doc.removed, 0);
        let doc = build("a\nb\n", "", None);
        assert_eq!(doc.removed, 2);
    }

    #[test]
    fn identical_input_is_flagged() {
        let doc = build("a\nb\n", "a\nb\n", Some(3));
        assert_eq!(doc.body, Body::Identical);
        assert_eq!(doc.rows.len(), 2);
    }

    #[test]
    fn file_without_trailing_newline_keeps_its_last_line() {
        let doc = build("a\nb", "a\nc", None);
        assert_eq!(doc.rows.len(), 2);
        assert_eq!(doc.rows[1].new.as_ref().unwrap().text, "c");
    }
}
