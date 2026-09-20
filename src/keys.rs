//! Neovim-style multi-key bindings.
//!
//! Bindings are written with the same notation as `vim.keymap.set`, so what is
//! in the table below reads like an nvim config. Resolution follows nvim's
//! rules: a sequence that is both a complete mapping and the prefix of a longer
//! one waits for the next key, and falls back to the shorter mapping if that
//! key does not extend it. That is what lets `<Space>` toggle a tree node while
//! still serving as the leader.

use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const LEADER: KeyCode = KeyCode::Char(' ');

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        // A capital letter arrives as Char('A') with SHIFT on some terminals
        // and without it on others; normalise so bindings match either way.
        let mods = match code {
            KeyCode::Char(_) => mods & !KeyModifiers::SHIFT,
            _ => mods,
        };
        Self { code, mods }
    }

    pub fn from_event(ev: KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers)
    }

    /// Render in nvim notation, for the status line and the help screen.
    pub fn render(self) -> String {
        let mut s = String::new();
        let ctrl = self.mods.contains(KeyModifiers::CONTROL);
        let alt = self.mods.contains(KeyModifiers::ALT);
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "CR".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "S-Tab".into(),
            KeyCode::Backspace => "BS".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        let bare = !ctrl && !alt && base.len() == 1;
        if bare {
            s.push_str(&base);
        } else {
            let _ = write!(
                s,
                "<{}{}{}>",
                if ctrl { "C-" } else { "" },
                if alt { "M-" } else { "" },
                base
            );
        }
        s
    }
}

/// Parse nvim-style notation: `<leader>fw`, `<C-d>`, `]g`, `<CR>`.
pub fn parse(spec: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(end) = chars[i..].iter().position(|&c| c == '>') {
                let name: String = chars[i + 1..i + end].iter().collect();
                if let Some(k) = parse_named(&name) {
                    keys.push(k);
                    i += end + 1;
                    continue;
                }
            }
        }
        keys.push(Key::new(KeyCode::Char(chars[i]), KeyModifiers::NONE));
        i += 1;
    }
    keys
}

fn parse_named(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    if lower == "leader" {
        return Some(Key::new(LEADER, KeyModifiers::NONE));
    }
    if let Some(rest) = lower.strip_prefix("c-") {
        let c = rest.chars().next()?;
        return Some(Key::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }
    if let Some(rest) = lower.strip_prefix("m-") {
        let c = rest.chars().next()?;
        return Some(Key::new(KeyCode::Char(c), KeyModifiers::ALT));
    }
    let code = match lower.as_str() {
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "s-tab" => KeyCode::BackTab,
        "bs" | "backspace" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        "bslash" => KeyCode::Char('\\'),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ => return None,
    };
    Some(Key::new(code, KeyModifiers::NONE))
}

/// Everything the UI can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    Refresh,

    // Panes
    ToggleTree,
    FocusTree,
    FocusDiff,
    ToggleLayout,
    WidenTree,
    NarrowTree,

    // Diff movement
    Down,
    Up,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Center,
    NextHunk,
    PrevHunk,
    NextFile,
    PrevFile,
    NextQuickfix,
    PrevQuickfix,
    ScrollLeft,
    ScrollRight,
    ScrollHome,

    // Folds / context
    ToggleFullContext,
    ToggleGroupDirs,

    // Search
    SearchPrompt,
    SearchNext,
    SearchPrev,
    ClearSearch,

    // Pickers (telescope)
    PickFiles,
    PickGrep,
    PickGrepWord,
    PickBuffers,
    PickLines,
    PickResume,

    // Tree (neo-tree)
    TreeOpen,
    TreeToggleNode,
    TreeCloseNode,
    TreeToggleAll,
    TreeNavigateUp,
    TreeFilter,
    TreeClearFilter,

    // Misc
    OpenEditor,
    YankPath,
    SwapSides,
}

/// Which key table is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Diff,
    Tree,
}

pub struct Keymap {
    diff: Vec<(Vec<Key>, Action)>,
    tree: Vec<(Vec<Key>, Action)>,
}

/// Bindings shared by both panes. Mirrors the user's nvim keymaps.
const SHARED: &[(&str, Action)] = &[
    ("q", Action::Quit),
    ("<C-c>", Action::Quit),
    ("?", Action::Help),
    ("R", Action::Refresh),
    // Telescope pickers
    ("<leader>j", Action::PickFiles),
    ("<leader>k", Action::PickGrep),
    ("<leader>fw", Action::PickGrepWord),
    ("<leader>fb", Action::PickBuffers),
    ("<leader>fs", Action::PickLines),
    ("<leader>fr", Action::PickResume),
    // neo-tree
    ("<leader>n", Action::ToggleTree),
    ("<bslash>", Action::ToggleTree),
    ("]g", Action::NextFile),
    ("[g", Action::PrevFile),
    // Window movement
    ("<C-w>h", Action::FocusTree),
    ("<C-w>l", Action::FocusDiff),
    ("<C-w><", Action::NarrowTree),
    ("<C-w>>", Action::WidenTree),
    ("<Tab>", Action::ToggleLayout),
    ("]q", Action::NextQuickfix),
    ("[q", Action::PrevQuickfix),
    ("<leader>g", Action::ToggleGroupDirs),
];

const DIFF_ONLY: &[(&str, Action)] = &[
    ("j", Action::Down),
    ("<Down>", Action::Down),
    ("k", Action::Up),
    ("<Up>", Action::Up),
    ("<C-d>", Action::HalfPageDown),
    ("<C-u>", Action::HalfPageUp),
    ("<C-f>", Action::PageDown),
    ("<C-b>", Action::PageUp),
    ("<PageDown>", Action::PageDown),
    ("<PageUp>", Action::PageUp),
    ("gg", Action::Top),
    ("G", Action::Bottom),
    ("zz", Action::Center),
    ("]c", Action::NextHunk),
    ("[c", Action::PrevHunk),
    ("za", Action::ToggleFullContext),
    ("/", Action::SearchPrompt),
    ("n", Action::SearchNext),
    ("N", Action::SearchPrev),
    ("<Esc>", Action::ClearSearch),
    ("e", Action::OpenEditor),
    ("<CR>", Action::OpenEditor),
    ("Y", Action::YankPath),
    ("s", Action::SwapSides),
    ("h", Action::ScrollLeft),
    ("<Left>", Action::ScrollLeft),
    ("l", Action::ScrollRight),
    ("<Right>", Action::ScrollRight),
    ("0", Action::ScrollHome),
];

const TREE_ONLY: &[(&str, Action)] = &[
    ("j", Action::Down),
    ("<Down>", Action::Down),
    ("k", Action::Up),
    ("<Up>", Action::Up),
    ("gg", Action::Top),
    ("G", Action::Bottom),
    ("<CR>", Action::TreeOpen),
    ("l", Action::TreeOpen),
    ("<Space>", Action::TreeToggleNode),
    ("C", Action::TreeCloseNode),
    ("h", Action::TreeCloseNode),
    ("z", Action::TreeToggleAll),
    ("<BS>", Action::TreeNavigateUp),
    ("/", Action::TreeFilter),
    ("<C-x>", Action::TreeClearFilter),
    ("e", Action::OpenEditor),
    ("Y", Action::YankPath),
];

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    pub fn new() -> Self {
        let build = |extra: &[(&str, Action)]| {
            let mut v: Vec<(Vec<Key>, Action)> = Vec::new();
            for (spec, action) in extra.iter().chain(SHARED.iter()) {
                let keys = parse(spec);
                // First definition wins, so a pane-specific binding overrides
                // the shared one with the same prefix.
                if !v.iter().any(|(k, _)| *k == keys) {
                    v.push((keys, *action));
                }
            }
            v
        };
        Self {
            diff: build(DIFF_ONLY),
            tree: build(TREE_ONLY),
        }
    }

    fn table(&self, ctx: Context) -> &[(Vec<Key>, Action)] {
        match ctx {
            Context::Diff => &self.diff,
            Context::Tree => &self.tree,
        }
    }

    /// All bindings in a context, for the help screen.
    pub fn entries(&self, ctx: Context) -> impl Iterator<Item = (String, Action)> + '_ {
        self.table(ctx).iter().map(|(keys, action)| {
            (
                keys.iter().map(|k| k.render()).collect::<String>(),
                *action,
            )
        })
    }
}

/// Outcome of feeding one more key to the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Run this action.
    Action(Action),
    /// A longer binding might still match; keep the pending buffer.
    Pending,
    /// Nothing matches; the buffer has been discarded.
    None,
    /// The pending buffer resolved to `action`, and `replay` still needs
    /// handling as a fresh keystroke. This is nvim's `nowait = false`.
    ActionThenReplay(Action, Key),
}

/// Feed `key` to the sequence resolver.
///
/// `pending` is the buffer of keys typed so far; this function updates it.
pub fn resolve(map: &Keymap, ctx: Context, pending: &mut Vec<Key>, key: Key) -> Resolved {
    pending.push(key);
    let table = map.table(ctx);

    let exact = table
        .iter()
        .find(|(keys, _)| keys.as_slice() == pending.as_slice())
        .map(|(_, a)| *a);
    let has_longer = table
        .iter()
        .any(|(keys, _)| keys.len() > pending.len() && keys.starts_with(pending));

    match (exact, has_longer) {
        // Unambiguous match.
        (Some(action), false) => {
            pending.clear();
            Resolved::Action(action)
        }
        // Complete, but a longer binding shares this prefix: wait one key.
        (Some(_), true) => Resolved::Pending,
        // Only a prefix so far.
        (None, true) => Resolved::Pending,
        (None, false) => {
            // The buffer is dead. If a proper prefix of it was a complete
            // binding, fire that and replay the key that broke the sequence.
            if pending.len() > 1 {
                let head = &pending[..pending.len() - 1];
                if let Some(action) = table
                    .iter()
                    .find(|(keys, _)| keys.as_slice() == head)
                    .map(|(_, a)| *a)
                {
                    pending.clear();
                    return Resolved::ActionThenReplay(action, key);
                }
            }
            pending.clear();
            Resolved::None
        }
    }
}

/// The longest prefix of `pending` that is a complete binding, if any.
///
/// Used when the timeout expires with a sequence still open.
pub fn longest_complete(map: &Keymap, ctx: Context, pending: &[Key]) -> Option<Action> {
    let table = map.table(ctx);
    (1..=pending.len()).rev().find_map(|n| {
        table
            .iter()
            .find(|(keys, _)| keys.as_slice() == &pending[..n])
            .map(|(_, a)| *a)
    })
}

/// Render the pending buffer for the status line, like nvim's showcmd.
pub fn render_pending(pending: &[Key]) -> String {
    pending.iter().map(|k| k.render()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn parses_nvim_notation() {
        assert_eq!(parse("<leader>fw"), vec![k(' '), k('f'), k('w')]);
        assert_eq!(parse("<C-d>"), vec![ctrl('d')]);
        assert_eq!(parse("]g"), vec![k(']'), k('g')]);
        assert_eq!(parse("<CR>"), vec![Key::new(KeyCode::Enter, KeyModifiers::NONE)]);
        assert_eq!(parse("<bslash>"), vec![k('\\')]);
    }

    #[test]
    fn renders_back_to_notation() {
        assert_eq!(ctrl('d').render(), "<C-d>");
        assert_eq!(k('j').render(), "j");
        assert_eq!(k(' ').render(), "<Space>");
    }

    #[test]
    fn leader_sequence_resolves_after_all_keys() {
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(resolve(&map, Context::Diff, &mut p, k(' ')), Resolved::Pending);
        assert_eq!(resolve(&map, Context::Diff, &mut p, k('f')), Resolved::Pending);
        assert_eq!(
            resolve(&map, Context::Diff, &mut p, k('w')),
            Resolved::Action(Action::PickGrepWord)
        );
        assert!(p.is_empty());
    }

    #[test]
    fn single_key_fires_immediately() {
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(
            resolve(&map, Context::Diff, &mut p, k('j')),
            Resolved::Action(Action::Down)
        );
    }

    #[test]
    fn space_toggles_a_tree_node_when_nothing_extends_it() {
        // nvim's `nowait = false`: <Space> is both a mapping and the leader.
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(resolve(&map, Context::Tree, &mut p, k(' ')), Resolved::Pending);
        // `x` extends nothing, so <Space> falls back to toggle_node and `x`
        // is replayed on its own.
        assert_eq!(
            resolve(&map, Context::Tree, &mut p, k('x')),
            Resolved::ActionThenReplay(Action::TreeToggleNode, k('x'))
        );
    }

    #[test]
    fn space_still_works_as_leader_in_the_tree() {
        let map = Keymap::new();
        let mut p = Vec::new();
        resolve(&map, Context::Tree, &mut p, k(' '));
        resolve(&map, Context::Tree, &mut p, k('f'));
        assert_eq!(
            resolve(&map, Context::Tree, &mut p, k('b')),
            Resolved::Action(Action::PickBuffers)
        );
    }

    #[test]
    fn unknown_sequences_are_discarded() {
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(resolve(&map, Context::Diff, &mut p, k('Q')), Resolved::None);
        assert!(p.is_empty());
    }

    #[test]
    fn z_folds_the_tree_without_waiting() {
        // Nothing in the tree's table starts with `z` except `z` itself, so it
        // must fire at once rather than sit through the sequence timeout.
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(
            resolve(&map, Context::Tree, &mut p, k('z')),
            Resolved::Action(Action::TreeToggleAll)
        );
        assert!(p.is_empty());
    }

    #[test]
    fn z_still_starts_a_sequence_in_the_diff_pane() {
        // There `zz` and `za` exist, so `z` alone means nothing yet.
        let map = Keymap::new();
        let mut p = Vec::new();
        assert_eq!(resolve(&map, Context::Diff, &mut p, k('z')), Resolved::Pending);
        assert_eq!(
            resolve(&map, Context::Diff, &mut p, k('a')),
            Resolved::Action(Action::ToggleFullContext)
        );
    }

    #[test]
    fn timeout_falls_back_to_the_longest_complete_prefix() {
        let map = Keymap::new();
        // <Space> alone toggles a tree node once the timeout expires.
        assert_eq!(
            longest_complete(&map, Context::Tree, &[k(' ')]),
            Some(Action::TreeToggleNode)
        );
        // In the diff pane <Space> is only ever a leader, so nothing fires.
        assert_eq!(longest_complete(&map, Context::Diff, &[k(' ')]), None);
        // A half-typed leader sequence also resolves to nothing.
        assert_eq!(longest_complete(&map, Context::Diff, &[k(' '), k('f')]), None);
    }

    #[test]
    fn bracket_g_moves_between_files_in_both_panes() {
        let map = Keymap::new();
        for ctx in [Context::Diff, Context::Tree] {
            let mut p = Vec::new();
            resolve(&map, ctx, &mut p, k(']'));
            assert_eq!(
                resolve(&map, ctx, &mut p, k('g')),
                Resolved::Action(Action::NextFile)
            );
        }
    }
}
