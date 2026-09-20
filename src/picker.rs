//! Telescope-style floating picker.
//!
//! Two flavours share one widget. A *static* picker (files, buffers, lines)
//! holds its whole candidate set and fuzzy-filters it locally with nucleo. A
//! *live* picker (grep) re-queries its backend on every keystroke and shows the
//! results in backend order.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// What selecting an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Open this changed file.
    File(String),
    /// Open this file and put the cursor on a line of the new side.
    Location { path: String, line: usize },
    /// Jump to a row of the document already on screen.
    Row(usize),
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// The text that is fuzzy-matched and highlighted.
    pub display: String,
    /// Dimmed trailing context: a status badge, a line number, a path.
    pub detail: String,
    pub target: Target,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub candidate: Candidate,
    /// Char indices into `candidate.display` that the query matched.
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Files,
    Buffers,
    Lines,
    Grep,
}

impl Kind {
    /// Live pickers ask their backend again on every keystroke.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Grep)
    }
}

pub struct Picker {
    pub kind: Kind,
    pub title: String,
    pub prompt: String,
    pub selected: usize,
    /// Scroll offset of the results list.
    pub offset: usize,
    items: Vec<Item>,
    /// Unfiltered candidates; empty for live pickers.
    source: Vec<Candidate>,
    matcher: Matcher,
}

impl Picker {
    pub fn new(kind: Kind, title: impl Into<String>, source: Vec<Candidate>) -> Self {
        let mut config = Config::DEFAULT;
        // Bonus for matches after `/`, so `s/m/main` finds src/main.rs.
        config.set_match_paths();
        let mut p = Self {
            kind,
            title: title.into(),
            prompt: String::new(),
            selected: 0,
            offset: 0,
            items: Vec::new(),
            source,
            matcher: Matcher::new(config),
        };
        if !kind.is_live() {
            p.refilter();
        }
        p
    }

    /// Seed the prompt, as `<leader>fw` does with the word under the cursor.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        if !self.kind.is_live() {
            self.refilter();
        }
        self
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn selected_target(&self) -> Option<&Target> {
        self.items.get(self.selected).map(|i| &i.candidate.target)
    }

    /// Every currently listed target -- what `<C-q>` sends to the quickfix list.
    pub fn all_targets(&self) -> Vec<Target> {
        self.items
            .iter()
            .map(|i| i.candidate.target.clone())
            .collect()
    }

    /// Replace the results of a live picker.
    pub fn set_items(&mut self, candidates: Vec<Candidate>) {
        self.items = candidates
            .into_iter()
            .map(|candidate| Item {
                candidate,
                indices: Vec::new(),
            })
            .collect();
        self.clamp();
    }

    pub fn insert(&mut self, c: char) {
        self.prompt.push(c);
        self.on_prompt_change();
    }

    pub fn backspace(&mut self) {
        self.prompt.pop();
        self.on_prompt_change();
    }

    /// `<C-u>`: the user's telescope config frees this key to clear the prompt.
    pub fn clear_prompt(&mut self) {
        self.prompt.clear();
        self.on_prompt_change();
    }

    /// `<C-w>`: delete the word before the cursor, as readline does.
    pub fn delete_word(&mut self) {
        let trimmed = self.prompt.trim_end();
        let cut = trimmed
            .rfind(|c: char| c.is_whitespace() || c == '/')
            .map_or(0, |i| i + 1);
        self.prompt.truncate(cut);
        self.on_prompt_change();
    }

    fn on_prompt_change(&mut self) {
        self.selected = 0;
        self.offset = 0;
        if !self.kind.is_live() {
            self.refilter();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        // Telescope wraps at both ends.
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    fn clamp(&mut self) {
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
    }

    /// Keep the selected row inside a window `height` rows tall.
    pub fn scroll_into_view(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
    }

    fn refilter(&mut self) {
        if self.prompt.is_empty() {
            self.items = self
                .source
                .iter()
                .cloned()
                .map(|candidate| Item {
                    candidate,
                    indices: Vec::new(),
                })
                .collect();
            self.clamp();
            return;
        }

        let pattern = Pattern::parse(&self.prompt, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, Item)> = Vec::new();

        for candidate in &self.source {
            let mut indices = Vec::new();
            let haystack = Utf32Str::new(&candidate.display, &mut buf);
            if let Some(score) = pattern.indices(haystack, &mut self.matcher, &mut indices) {
                indices.sort_unstable();
                indices.dedup();
                scored.push((
                    score,
                    Item {
                        candidate: candidate.clone(),
                        indices,
                    },
                ));
            }
        }

        // Highest score first; ties keep source order so the list is stable.
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.items = scored.into_iter().map(|(_, item)| item).collect();
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<Candidate> {
        paths
            .iter()
            .map(|p| Candidate {
                display: p.to_string(),
                detail: "M".into(),
                target: Target::File(p.to_string()),
            })
            .collect()
    }

    #[test]
    fn empty_prompt_lists_everything() {
        let p = Picker::new(Kind::Files, "Files", files(&["a.rs", "b.rs"]));
        assert_eq!(p.items().len(), 2);
    }

    #[test]
    fn fuzzy_matches_across_path_segments() {
        let mut p = Picker::new(
            Kind::Files,
            "Files",
            files(&["src/main.rs", "docs/readme.md", "src/ui/split.rs"]),
        );
        for c in "srmain".chars() {
            p.insert(c);
        }
        assert_eq!(p.items().len(), 1);
        assert_eq!(p.items()[0].candidate.display, "src/main.rs");
        assert!(!p.items()[0].indices.is_empty());
    }

    #[test]
    fn non_matches_are_dropped() {
        let mut p = Picker::new(Kind::Files, "Files", files(&["a.rs"]));
        for c in "zzzz".chars() {
            p.insert(c);
        }
        assert!(p.items().is_empty());
        assert!(p.selected_target().is_none());
    }

    #[test]
    fn ctrl_u_clears_the_prompt_and_restores_the_list() {
        let mut p = Picker::new(Kind::Files, "Files", files(&["a.rs", "b.rs"]));
        p.insert('a');
        assert_eq!(p.items().len(), 1);
        p.clear_prompt();
        assert_eq!(p.items().len(), 2);
        assert!(p.prompt.is_empty());
    }

    #[test]
    fn delete_word_cuts_back_to_a_separator() {
        let mut p = Picker::new(Kind::Files, "Files", files(&["a.rs"]));
        p.prompt = "src/main".into();
        p.delete_word();
        assert_eq!(p.prompt, "src/");
    }

    #[test]
    fn selection_wraps_like_telescope() {
        let mut p = Picker::new(Kind::Files, "Files", files(&["a.rs", "b.rs", "c.rs"]));
        p.move_selection(-1);
        assert_eq!(p.selected, 2);
        p.move_selection(1);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn live_pickers_keep_backend_order() {
        let mut p = Picker::new(Kind::Grep, "Grep", Vec::new());
        p.insert('x');
        p.set_items(files(&["z.rs", "a.rs"]));
        assert_eq!(p.items()[0].candidate.display, "z.rs");
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let mut p = Picker::new(
            Kind::Files,
            "Files",
            files(&["a", "b", "c", "d", "e", "f", "g"]),
        );
        p.selected = 6;
        p.scroll_into_view(3);
        assert_eq!(p.offset, 4);
        p.selected = 0;
        p.scroll_into_view(3);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn seeded_prompt_filters_immediately() {
        let p = Picker::new(Kind::Files, "Files", files(&["alpha.rs", "beta.rs"]))
            .with_prompt("alpha");
        assert_eq!(p.items().len(), 1);
    }
}
