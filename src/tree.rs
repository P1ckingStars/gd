//! The changed-files tree in the left pane.
//!
//! Modelled on neo-tree: directories expand and collapse, indent guides show
//! nesting, and every row carries the git status of what it holds.

use std::collections::{BTreeMap, HashSet};

use crate::git::{FileEntry, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    /// Index into the file list the tree was built from.
    File(usize),
}

/// One rendered row of the tree.
#[derive(Debug, Clone)]
pub struct Node {
    pub depth: usize,
    /// Just the segment(s) shown on this row; grouped chains keep their slashes.
    pub name: String,
    /// Full repo-relative path, and the key used for expansion state.
    pub path: String,
    pub kind: NodeKind,
    pub expanded: bool,
    /// Whether this row is the last child of its parent, for indent guides.
    pub last_child: bool,
    /// For each ancestor level, whether that ancestor was its parent's last
    /// child -- which is what decides between a rail and blank space.
    pub guides: Vec<bool>,
    /// For directories: how many changed files live underneath.
    pub count: usize,
    pub status: Option<Status>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

/// Intermediate nested form, built fresh on every reload.
#[derive(Default, Debug)]
struct Dir {
    subdirs: BTreeMap<String, Dir>,
    files: BTreeMap<String, usize>,
    count: usize,
}

pub struct Tree {
    root: Dir,
    collapsed: HashSet<String>,
    /// Flattened rows in display order; rebuilt whenever the shape changes.
    pub nodes: Vec<Node>,
    pub selected: usize,
    /// Substring filter applied by the tree's own `/` finder.
    filter: String,
    /// Fold `a/b/c` into one row when each level has a single child.
    pub group_dirs: bool,
}

impl Tree {
    pub fn new(files: &[FileEntry]) -> Self {
        let mut t = Self {
            root: Dir::default(),
            collapsed: HashSet::new(),
            nodes: Vec::new(),
            selected: 0,
            filter: String::new(),
            group_dirs: true,
        };
        t.rebuild_from(files);
        t
    }

    /// Replace the contents, keeping expansion state and (if possible) the
    /// selected path across a reload.
    pub fn reload(&mut self, files: &[FileEntry]) {
        let previous = self.selected_path().map(str::to_string);
        self.rebuild_from(files);
        if let Some(p) = previous {
            self.reveal(&p);
        }
    }

    fn rebuild_from(&mut self, files: &[FileEntry]) {
        self.root = Dir::default();
        for (i, f) in files.iter().enumerate() {
            insert(&mut self.root, &f.path, i);
        }
        self.flatten(files);
    }

    pub fn set_filter(&mut self, filter: &str, files: &[FileEntry]) {
        self.filter = filter.to_lowercase();
        self.flatten(files);
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn toggle_group_dirs(&mut self, files: &[FileEntry]) {
        self.group_dirs = !self.group_dirs;
        let previous = self.selected_path().map(str::to_string);
        self.flatten(files);
        if let Some(p) = previous {
            self.reveal(&p);
        }
    }

    fn flatten(&mut self, files: &[FileEntry]) {
        let mut out = Vec::new();
        // Borrow-splitting: walk reads `root`, so hand it only what it needs.
        let root = std::mem::take(&mut self.root);
        walk(
            &root,
            "",
            0,
            &[],
            &self.collapsed,
            &self.filter,
            self.group_dirs,
            files,
            &mut out,
        );
        self.root = root;
        self.nodes = out;
        if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len().saturating_sub(1);
        }
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.nodes.get(self.selected)
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.selected_node().map(|n| n.path.as_str())
    }

    /// The file index under the cursor, if the cursor is on a file.
    pub fn selected_file(&self) -> Option<usize> {
        match self.selected_node()?.kind {
            NodeKind::File(i) => Some(i),
            NodeKind::Dir => None,
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.nodes.is_empty() {
            return;
        }
        let last = self.nodes.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.nodes.len().saturating_sub(1);
    }

    /// Move to the next (or previous) *file* row, skipping directories.
    /// This is what `]g` / `[g` drive.
    pub fn step_file(&mut self, forward: bool) -> Option<usize> {
        if self.nodes.is_empty() {
            return None;
        }
        let len = self.nodes.len();
        let mut i = self.selected;
        for _ in 0..len {
            i = if forward {
                (i + 1) % len
            } else {
                (i + len - 1) % len
            };
            if let NodeKind::File(f) = self.nodes[i].kind {
                self.selected = i;
                return Some(f);
            }
        }
        None
    }

    pub fn toggle(&mut self, files: &[FileEntry]) {
        let Some(node) = self.selected_node() else {
            return;
        };
        if !node.is_dir() {
            return;
        }
        let path = node.path.clone();
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.flatten(files);
    }

    /// neo-tree's `C`: collapse the directory under the cursor, or the parent
    /// directory when the cursor is already on a file.
    pub fn close_node(&mut self, files: &[FileEntry]) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let target = if node.is_dir() {
            node.path.clone()
        } else {
            match parent_of(&node.path) {
                Some(p) => p.to_string(),
                None => return,
            }
        };
        self.collapsed.insert(target.clone());
        self.flatten(files);
        self.reveal(&target);
    }

    /// neo-tree's `z`.
    pub fn close_all(&mut self, files: &[FileEntry]) {
        let selected = self.selected_path().map(str::to_string);
        for node in &self.nodes {
            if node.is_dir() {
                self.collapsed.insert(node.path.clone());
            }
        }
        // Collect first, then flatten, since the loop borrowed `self.nodes`.
        self.flatten(files);
        if let Some(p) = selected {
            // The old selection may now be hidden; settle on its nearest
            // visible ancestor.
            let mut probe = p.as_str();
            loop {
                if self.reveal_shallow(probe) {
                    break;
                }
                match parent_of(probe) {
                    Some(p) => probe = p,
                    None => break,
                }
            }
        }
    }

    pub fn expand_all(&mut self, files: &[FileEntry]) {
        self.collapsed.clear();
        self.flatten(files);
    }

    /// One key for both directions: collapse everything if anything is open,
    /// otherwise open everything. Returns what it did, or `None` when the
    /// tree is flat and there is nothing to fold.
    pub fn toggle_all(&mut self, files: &[FileEntry]) -> Option<bool> {
        if !self.nodes.iter().any(Node::is_dir) {
            return None;
        }
        // Only visible rows are checked, which is the point: once the top
        // level is collapsed nothing below it counts as open.
        let expanded = self.nodes.iter().any(|n| n.is_dir() && n.expanded);
        if expanded {
            self.close_all(files);
        } else {
            self.expand_all(files);
        }
        Some(!expanded)
    }

    /// neo-tree's `<bs>`: jump to the parent directory row.
    pub fn navigate_up(&mut self) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let Some(parent) = parent_of(&node.path) else {
            return;
        };
        let parent = parent.to_string();
        self.reveal_shallow(&parent);
    }

    /// Expand every ancestor of `path` and put the cursor on it.
    pub fn reveal(&mut self, path: &str) -> bool {
        let mut changed = false;
        let mut acc = String::new();
        for seg in path.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            if self.collapsed.remove(&acc) {
                changed = true;
            }
        }
        if changed {
            // `flatten` needs the file list only for status badges, which
            // reveal does not alter, so re-derive rows from the current tree.
            let files: Vec<FileEntry> = Vec::new();
            let _ = &files;
            self.reflatten_preserving();
        }
        self.reveal_shallow(path)
    }

    /// Put the cursor on `path` if it is currently visible.
    fn reveal_shallow(&mut self, path: &str) -> bool {
        if let Some(i) = self.nodes.iter().position(|n| n.path == path) {
            self.selected = i;
            true
        } else {
            false
        }
    }

    /// Re-flatten without a file list: node statuses are already cached on the
    /// rows, so we rebuild from the stored `Dir` and patch statuses back.
    fn reflatten_preserving(&mut self) {
        let statuses: BTreeMap<String, Status> = self
            .nodes
            .iter()
            .filter_map(|n| n.status.map(|s| (n.path.clone(), s)))
            .collect();
        let root = std::mem::take(&mut self.root);
        let mut out = Vec::new();
        walk(
            &root,
            "",
            0,
            &[],
            &self.collapsed,
            &self.filter,
            self.group_dirs,
            &[],
            &mut out,
        );
        self.root = root;
        for node in &mut out {
            node.status = statuses.get(&node.path).copied();
        }
        self.nodes = out;
    }
}

fn parent_of(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(head, _)| head)
}

fn insert(dir: &mut Dir, path: &str, index: usize) {
    dir.count += 1;
    match path.split_once('/') {
        Some((head, rest)) => {
            let child = dir.subdirs.entry(head.to_string()).or_default();
            insert(child, rest, index);
        }
        None => {
            dir.files.insert(path.to_string(), index);
        }
    }
}

/// Flatten `dir` into display rows.
///
/// `files` may be empty when only the shape changed; statuses are then patched
/// in by the caller.
#[allow(clippy::too_many_arguments)]
fn walk(
    dir: &Dir,
    prefix: &str,
    depth: usize,
    guides: &[bool],
    collapsed: &HashSet<String>,
    filter: &str,
    group_dirs: bool,
    files: &[FileEntry],
    out: &mut Vec<Node>,
) {
    let total = dir.subdirs.len() + dir.files.len();
    let mut seen = 0;

    for (name, sub) in &dir.subdirs {
        seen += 1;
        let last_child = seen == total;

        // Fold `a/b/c` into a single row while each level holds exactly one
        // subdirectory and no files of its own.
        let mut label = name.clone();
        let mut path = join(prefix, name);
        let mut node = sub;
        if group_dirs {
            while node.files.is_empty() && node.subdirs.len() == 1 {
                let (child_name, child) = node.subdirs.iter().next().unwrap();
                label = format!("{label}/{child_name}");
                path = join(&path, child_name);
                node = child;
            }
        }

        let expanded = !collapsed.contains(&path);
        // A filter hides directories with no surviving descendants.
        let matches_below = filter.is_empty() || dir_matches(node, &path, filter);
        if !matches_below {
            continue;
        }

        out.push(Node {
            depth,
            name: label,
            path: path.clone(),
            kind: NodeKind::Dir,
            expanded,
            last_child,
            guides: guides.to_vec(),
            count: node.count,
            status: None,
        });

        if expanded {
            let mut nested = guides.to_vec();
            nested.push(last_child);
            walk(
                node,
                &path,
                depth + 1,
                &nested,
                collapsed,
                filter,
                group_dirs,
                files,
                out,
            );
        }
    }

    for (name, &index) in &dir.files {
        seen += 1;
        let path = join(prefix, name);
        if !filter.is_empty() && !path.to_lowercase().contains(filter) {
            continue;
        }
        out.push(Node {
            depth,
            name: name.clone(),
            path,
            kind: NodeKind::File(index),
            expanded: false,
            last_child: seen == total,
            guides: guides.to_vec(),
            count: 0,
            status: files.get(index).map(|f| f.status),
        });
    }
}

/// Does anything under `dir` match the filter?
fn dir_matches(dir: &Dir, prefix: &str, filter: &str) -> bool {
    if prefix.to_lowercase().contains(filter) {
        return true;
    }
    if dir
        .files
        .keys()
        .any(|f| join(prefix, f).to_lowercase().contains(filter))
    {
        return true;
    }
    dir.subdirs
        .iter()
        .any(|(name, sub)| dir_matches(sub, &join(prefix, name), filter))
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(paths: &[&str]) -> Vec<FileEntry> {
        paths
            .iter()
            .map(|p| FileEntry {
                path: p.to_string(),
                old_path: p.to_string(),
                status: Status::Modified,
            })
            .collect()
    }

    #[test]
    fn nests_directories_and_files() {
        let files = entries(&["src/a.rs", "src/b.rs", "README.md"]);
        let t = Tree::new(&files);
        let paths: Vec<&str> = t.nodes.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, vec!["src", "src/a.rs", "src/b.rs", "README.md"]);
    }

    #[test]
    fn groups_single_child_directory_chains() {
        let files = entries(&["a/b/c/deep.rs"]);
        let t = Tree::new(&files);
        assert_eq!(t.nodes[0].name, "a/b/c");
        assert_eq!(t.nodes[0].path, "a/b/c");
        assert_eq!(t.nodes.len(), 2);
    }

    #[test]
    fn grouping_can_be_turned_off() {
        let files = entries(&["a/b/c/deep.rs"]);
        let mut t = Tree::new(&files);
        t.toggle_group_dirs(&files);
        assert_eq!(t.nodes.len(), 4);
        assert_eq!(t.nodes[0].name, "a");
    }

    #[test]
    fn collapsing_hides_children() {
        let files = entries(&["src/a.rs", "src/b.rs"]);
        let mut t = Tree::new(&files);
        assert_eq!(t.nodes.len(), 3);
        t.selected = 0;
        t.toggle(&files);
        assert_eq!(t.nodes.len(), 1);
        t.toggle(&files);
        assert_eq!(t.nodes.len(), 3);
    }

    #[test]
    fn step_file_skips_directory_rows_and_wraps() {
        let files = entries(&["src/a.rs", "src/b.rs"]);
        let mut t = Tree::new(&files);
        t.selected = 0; // on the "src" directory
        assert_eq!(t.step_file(true), Some(0));
        assert_eq!(t.step_file(true), Some(1));
        // Wrapping past the end comes back to the first file.
        assert_eq!(t.step_file(true), Some(0));
        assert_eq!(t.step_file(false), Some(1));
    }

    #[test]
    fn toggle_all_alternates_between_the_two_extremes() {
        let files = entries(&["src/deep/x.rs", "src/y.rs", "other.rs"]);
        let mut t = Tree::new(&files);
        let open = t.nodes.len();

        assert_eq!(t.toggle_all(&files), Some(false));
        let shut = t.nodes.len();
        assert!(shut < open, "collapsing should hide rows");

        assert_eq!(t.toggle_all(&files), Some(true));
        assert_eq!(t.nodes.len(), open);
    }

    #[test]
    fn toggle_all_collapses_first_from_a_partly_open_tree() {
        let files = entries(&["a/one.rs", "b/two.rs"]);
        let mut t = Tree::new(&files);
        let open = t.nodes.len();
        // Collapse just one of the two directories.
        t.reveal("a");
        t.toggle(&files);
        assert!(t.nodes.len() < open);

        // Anything still open means the first press closes the rest.
        assert_eq!(t.toggle_all(&files), Some(false));
        assert!(!t.nodes.iter().any(|n| n.is_dir() && n.expanded));
        assert_eq!(t.toggle_all(&files), Some(true));
        assert_eq!(t.nodes.len(), open);
    }

    #[test]
    fn toggle_all_reports_nothing_to_do_on_a_flat_tree() {
        let files = entries(&["a.rs", "b.rs"]);
        let mut t = Tree::new(&files);
        assert_eq!(t.toggle_all(&files), None);
        assert_eq!(t.nodes.len(), 2);
    }

    #[test]
    fn reveal_expands_ancestors() {
        let files = entries(&["src/deep/x.rs", "other.rs"]);
        let mut t = Tree::new(&files);
        t.close_all(&files);
        assert!(!t.nodes.iter().any(|n| n.path == "src/deep/x.rs"));
        assert!(t.reveal("src/deep/x.rs"));
        assert_eq!(t.selected_path(), Some("src/deep/x.rs"));
    }

    #[test]
    fn filter_keeps_matching_paths_only() {
        let files = entries(&["src/alpha.rs", "src/beta.rs", "docs/alpha.md"]);
        let mut t = Tree::new(&files);
        t.set_filter("beta", &files);
        let paths: Vec<&str> = t.nodes.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, vec!["src", "src/beta.rs"]);
    }

    #[test]
    fn directory_rows_carry_a_count() {
        let files = entries(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let t = Tree::new(&files);
        assert_eq!(t.nodes[0].count, 3);
    }

    #[test]
    fn reload_keeps_the_cursor_on_the_same_file() {
        let files = entries(&["src/a.rs", "src/b.rs"]);
        let mut t = Tree::new(&files);
        t.reveal("src/b.rs");
        let more = entries(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        t.reload(&more);
        assert_eq!(t.selected_path(), Some("src/b.rs"));
    }
}
