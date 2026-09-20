//! Everything that talks to git.
//!
//! We shell out to the `git` binary rather than linking libgit2: it costs one
//! process per query, but it inherits the user's full git configuration
//! (rename detection, `diff.*` settings, pathspec rules, worktrees, submodule
//! handling) for free, and it keeps the build free of C dependencies.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// What the left and right panes are comparing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// `HEAD` (left) against the working tree (right).
    Worktree,
    /// `HEAD` (left) against the index (right) -- i.e. `--staged`.
    Staged,
    /// A revision (left) against the working tree (right).
    Rev(String),
    /// Two revisions.
    Range(String, String),
}

impl Spec {
    /// Label shown above the left pane.
    pub fn old_label(&self) -> String {
        match self {
            Self::Worktree | Self::Staged => "HEAD".into(),
            Self::Rev(r) => r.clone(),
            Self::Range(a, _) => a.clone(),
        }
    }

    /// Label shown above the right pane.
    pub fn new_label(&self) -> String {
        match self {
            Self::Worktree => "working tree".into(),
            Self::Staged => "index".into(),
            Self::Rev(_) => "working tree".into(),
            Self::Range(_, b) => b.clone(),
        }
    }

    /// The revisions this comparison names, for validation. The working tree
    /// and index sides are not revisions and need no check.
    pub fn revisions(&self) -> Vec<&str> {
        match self {
            Self::Worktree | Self::Staged => Vec::new(),
            Self::Rev(r) => vec![r.as_str()],
            Self::Range(a, b) => vec![a.as_str(), b.as_str()],
        }
    }

    /// The arguments that make `git diff` produce this comparison.
    fn diff_args(&self) -> Vec<String> {
        match self {
            Self::Worktree => vec!["HEAD".into()],
            Self::Staged => vec!["--cached".into(), "HEAD".into()],
            Self::Rev(r) => vec![r.clone()],
            Self::Range(a, b) => vec![a.clone(), b.clone()],
        }
    }

    /// True when the right-hand side is the on-disk working tree, which is the
    /// only case where untracked files are worth listing.
    fn right_is_worktree(&self) -> bool {
        matches!(self, Self::Worktree | Self::Rev(_))
    }
}

/// How a file changed between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
}

impl Status {
    fn from_code(code: &str) -> Self {
        match code.as_bytes().first() {
            Some(b'A') => Self::Added,
            Some(b'D') => Self::Deleted,
            Some(b'R') => Self::Renamed,
            Some(b'C') => Self::Copied,
            Some(b'T') => Self::TypeChanged,
            Some(b'U') => Self::Unmerged,
            _ => Self::Modified,
        }
    }

    /// Single-letter badge for the file tree, matching `git status` codes.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unmerged => "U",
            Self::Untracked => "?",
        }
    }
}

/// One changed file.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path on the new side -- what the tree displays and what we key on.
    pub path: String,
    /// Path on the old side. Differs from `path` only for renames and copies.
    pub old_path: String,
    pub status: Status,
}

/// One side of a file's contents, already classified.
#[derive(Debug, Clone)]
pub enum Blob {
    Text(String),
    Binary,
    /// The file does not exist on this side (added or deleted).
    Absent,
}

impl Blob {
    pub fn text(&self) -> &str {
        match self {
            Self::Text(s) => s,
            _ => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Find the repository containing `start`.
    pub fn discover(start: &Path) -> Result<Self> {
        let out = Command::new("git")
            .current_dir(start)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("failed to run `git` -- is it installed and on PATH?")?;
        if !out.status.success() {
            bail!("not inside a git repository");
        }
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Short description of where HEAD is, for the status line.
    pub fn head_label(&self) -> String {
        let branch = self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if branch.is_empty() || branch == "HEAD" {
            self.run(&["rev-parse", "--short", "HEAD"])
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "(no commits)".into())
        } else {
            branch
        }
    }

    /// Reject unusable revisions before the TUI takes over the terminal,
    /// where a raw git error would be painted over by the first frame.
    pub fn verify(&self, spec: &Spec) -> Result<()> {
        for rev in spec.revisions() {
            if self.resolves(rev) {
                continue;
            }
            let mut msg = format!("unknown revision `{rev}`");
            // `HEAD~N` nearly always comes from gd's own commit-count
            // shorthand, so answer the question the user actually has.
            if let Some(n) = rev.strip_prefix("HEAD~").and_then(|s| s.parse::<usize>().ok()) {
                let have = self.commit_count();
                msg.push_str(&format!(
                    "\n       this repository has {have} commit{}, so there is nothing {n} back",
                    if have == 1 { "" } else { "s" }
                ));
            }
            bail!(msg);
        }
        Ok(())
    }

    /// Does `rev` name a commit?
    fn resolves(&self, rev: &str) -> bool {
        Command::new("git")
            .current_dir(&self.root)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Commits reachable from HEAD; 0 in a repository with no commits.
    pub fn commit_count(&self) -> usize {
        self.run(&["rev-list", "--count", "HEAD"])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .context("failed to run git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Raw bytes, so we can tell text from binary ourselves.
    fn run_bytes(&self, args: &[&str]) -> Result<Option<Vec<u8>>> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .context("failed to run git")?;
        // A missing path is an expected outcome (added/deleted files), not an
        // error worth surfacing to the user.
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(out.stdout))
    }

    /// Every file that differs between the two sides of `spec`, sorted by path.
    pub fn changed_files(&self, spec: &Spec) -> Result<Vec<FileEntry>> {
        let mut args: Vec<String> = vec![
            "diff".into(),
            "--name-status".into(),
            "--find-renames".into(),
            "-z".into(),
            "--no-color".into(),
        ];
        args.extend(spec.diff_args());

        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        // An empty repository has no HEAD to diff against; treat every
        // untracked file as an addition instead of failing outright.
        let raw = match self.run(&argv) {
            Ok(s) => s,
            Err(e) if self.has_head().is_ok_and(|h| !h) => {
                if spec.right_is_worktree() {
                    String::new()
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };

        let mut files = parse_name_status(&raw);

        if spec.right_is_worktree() {
            for path in self.untracked()? {
                files.push(FileEntry {
                    old_path: path.clone(),
                    path,
                    status: Status::Untracked,
                });
            }
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Ok(files)
    }

    fn has_head(&self) -> Result<bool> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()?;
        Ok(out.status.success())
    }

    fn untracked(&self) -> Result<Vec<String>> {
        let raw = self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        Ok(raw
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Contents of the left pane for `file`.
    pub fn old_blob(&self, spec: &Spec, file: &FileEntry) -> Result<Blob> {
        if matches!(file.status, Status::Added | Status::Untracked) {
            return Ok(Blob::Absent);
        }
        let rev = match spec {
            Spec::Worktree | Spec::Staged => "HEAD".to_string(),
            Spec::Rev(r) => r.clone(),
            Spec::Range(a, _) => a.clone(),
        };
        self.show(&format!("{rev}:{}", file.old_path))
    }

    /// Contents of the right pane for `file`.
    pub fn new_blob(&self, spec: &Spec, file: &FileEntry) -> Result<Blob> {
        if file.status == Status::Deleted {
            return Ok(Blob::Absent);
        }
        match spec {
            // Untracked files have no index entry yet, so always read on-disk.
            Spec::Worktree | Spec::Rev(_) => self.read_worktree(&file.path),
            Spec::Staged => self.show(&format!(":{}", file.path)),
            Spec::Range(_, b) => self.show(&format!("{b}:{}", file.path)),
        }
    }

    fn show(&self, object: &str) -> Result<Blob> {
        match self.run_bytes(&["show", object])? {
            Some(bytes) => Ok(classify(bytes)),
            None => Ok(Blob::Absent),
        }
    }

    fn read_worktree(&self, path: &str) -> Result<Blob> {
        match std::fs::read(self.root.join(path)) {
            Ok(bytes) => Ok(classify(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Blob::Absent),
            Err(e) => Err(e).with_context(|| format!("reading {path}")),
        }
    }
}

/// git's own heuristic: a NUL byte anywhere in the first 8000 bytes means binary.
fn classify(bytes: Vec<u8>) -> Blob {
    let probe = &bytes[..bytes.len().min(8000)];
    if probe.contains(&0) {
        return Blob::Binary;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Blob::Text(text),
        Err(_) => Blob::Binary,
    }
}

/// Parse `git diff --name-status -z` output.
///
/// Records are NUL-separated. Most are `<code>\0<path>\0`, but renames and
/// copies carry a similarity score and span three fields:
/// `R100\0<old>\0<new>\0`.
fn parse_name_status(raw: &str) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let mut fields = raw.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = fields.next() {
        let status = Status::from_code(code);
        let (old_path, path) = if matches!(status, Status::Renamed | Status::Copied) {
            let Some(old) = fields.next() else { break };
            let Some(new) = fields.next() else { break };
            (old.to_string(), new.to_string())
        } else {
            let Some(p) = fields.next() else { break };
            (p.to_string(), p.to_string())
        };
        out.push(FileEntry {
            path,
            old_path,
            status,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_records() {
        let entries = parse_name_status("M\0src/main.rs\0A\0src/new.rs\0");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].status, Status::Modified);
        assert_eq!(entries[1].status, Status::Added);
    }

    #[test]
    fn parses_renames_as_three_fields() {
        let entries = parse_name_status("R096\0old/a.rs\0new/b.rs\0M\0c.rs\0");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, Status::Renamed);
        assert_eq!(entries[0].old_path, "old/a.rs");
        assert_eq!(entries[0].path, "new/b.rs");
        // The record after a rename must still line up.
        assert_eq!(entries[1].path, "c.rs");
    }

    #[test]
    fn truncated_record_does_not_panic() {
        assert!(parse_name_status("R100\0only-old\0").is_empty());
    }

    #[test]
    fn nul_byte_means_binary() {
        assert!(matches!(classify(vec![0x89, 0x50, 0x00, 0x4e]), Blob::Binary));
        assert!(matches!(classify(b"hello".to_vec()), Blob::Text(_)));
    }
}
