//! Colours, kept in one place so the palette can be swapped wholesale.
//!
//! The defaults follow Rose Pine, which is what the user's nvim runs, and
//! assume a dark terminal with true colour. Every style is a plain `Style`, so
//! dropping in another palette means editing only this file.

use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub text: Style,
    pub dim: Style,
    pub border: Style,
    pub border_focus: Style,
    pub title: Style,

    pub gutter: Style,
    pub context: Style,

    pub added: Style,
    pub removed: Style,
    /// Word-level highlight inside an otherwise-changed line.
    pub added_word: Style,
    pub removed_word: Style,
    /// Filler shown where one side has no line at all.
    pub filler: Style,
    pub separator: Style,

    pub selection: Style,
    pub match_hit: Style,
    pub prompt: Style,
    pub status: Style,
    pub status_accent: Style,
    pub warn: Style,

    pub badge_added: Style,
    pub badge_modified: Style,
    pub badge_deleted: Style,
    pub badge_untracked: Style,
    pub dir: Style,
}

// Rose Pine.
const TEXT: Color = Color::Rgb(224, 222, 244);
const SUBTLE: Color = Color::Rgb(144, 140, 170);
const MUTED: Color = Color::Rgb(110, 106, 134);
const OVERLAY: Color = Color::Rgb(38, 35, 58);
const HIGHLIGHT_MED: Color = Color::Rgb(64, 61, 82);
const LOVE: Color = Color::Rgb(235, 111, 146);
const GOLD: Color = Color::Rgb(246, 193, 119);
const PINE: Color = Color::Rgb(49, 116, 143);
const FOAM: Color = Color::Rgb(156, 207, 216);
const IRIS: Color = Color::Rgb(196, 167, 231);

// Diff backgrounds: dark enough to sit under normal text, distinct enough to
// scan at a glance.
const ADD_BG: Color = Color::Rgb(24, 46, 38);
const ADD_BG_STRONG: Color = Color::Rgb(38, 82, 64);
const DEL_BG: Color = Color::Rgb(54, 27, 38);
const DEL_BG_STRONG: Color = Color::Rgb(97, 42, 62);
const FILLER_BG: Color = Color::Rgb(27, 26, 38);

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Style::new().fg(TEXT),
            dim: Style::new().fg(MUTED),
            border: Style::new().fg(HIGHLIGHT_MED),
            border_focus: Style::new().fg(IRIS),
            title: Style::new().fg(FOAM).add_modifier(Modifier::BOLD),

            gutter: Style::new().fg(MUTED),
            context: Style::new().fg(SUBTLE),

            added: Style::new().fg(TEXT).bg(ADD_BG),
            removed: Style::new().fg(TEXT).bg(DEL_BG),
            added_word: Style::new().fg(TEXT).bg(ADD_BG_STRONG),
            removed_word: Style::new().fg(TEXT).bg(DEL_BG_STRONG),
            filler: Style::new().fg(MUTED).bg(FILLER_BG),
            separator: Style::new().fg(IRIS).bg(OVERLAY),

            selection: Style::new().bg(HIGHLIGHT_MED).add_modifier(Modifier::BOLD),
            match_hit: Style::new().fg(Color::Black).bg(GOLD),
            prompt: Style::new().fg(GOLD),
            status: Style::new().fg(SUBTLE).bg(OVERLAY),
            status_accent: Style::new().fg(IRIS).bg(OVERLAY).add_modifier(Modifier::BOLD),
            warn: Style::new().fg(GOLD).bg(OVERLAY),

            badge_added: Style::new().fg(FOAM),
            badge_modified: Style::new().fg(GOLD),
            badge_deleted: Style::new().fg(LOVE),
            badge_untracked: Style::new().fg(PINE),
            dir: Style::new().fg(IRIS),
        }
    }
}

impl Theme {
    pub fn badge(&self, status: crate::git::Status) -> Style {
        use crate::git::Status as S;
        match status {
            S::Added => self.badge_added,
            S::Deleted => self.badge_deleted,
            S::Untracked => self.badge_untracked,
            S::Unmerged => self.badge_deleted,
            _ => self.badge_modified,
        }
    }

    pub fn pane_border(&self, focused: bool) -> Style {
        if focused {
            self.border_focus
        } else {
            self.border
        }
    }
}
