//! `gd` -- a side-by-side git diff viewer for the terminal.

mod app;
mod clipboard;
mod diff;
mod git;
mod keys;
mod picker;
mod search;
mod theme;
mod tree;
mod ui;

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;

use crate::app::{App, Effect};
use crate::git::{Repo, Spec};
use crate::theme::Theme;

const USAGE: &str = "\
gd -- view git diffs side by side

USAGE:
    gd [OPTIONS] [REVISION]
    gd [OPTIONS] <REVISION>..<REVISION>
    gd [OPTIONS] <REVISION> <REVISION>

ARGS:
    <REVISION>          Compare this revision against the working tree.
    <REV>..<REV>        Compare two revisions.

OPTIONS:
    -s, --staged        Compare HEAD against the index (same as --cached).
        --cached        Alias for --staged.
    -h, --help          Print this help.
    -V, --version       Print the version.

Press ? inside gd for the full key list.
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let spec = match parse_args(&args) {
        Ok(Parsed::Spec(s)) => s,
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return Ok(());
        }
        Ok(Parsed::Version) => {
            println!("gd {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Err(msg) => {
            eprintln!("gd: {msg}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };

    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let repo = Repo::discover(&cwd)?;
    let app = App::new(repo, spec)?;

    let terminal = ratatui::init();
    let result = run(terminal, app);
    ratatui::restore();
    result
}

enum Parsed {
    Spec(Spec),
    Help,
    Version,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut staged = false;
    let mut revs: Vec<String> = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-s" | "--staged" | "--cached" => staged = true,
            // Everything after `--` is a revision, never a flag.
            "--" => revs.extend(iter.by_ref().cloned()),
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`"));
            }
            other => revs.push(other.to_string()),
        }
    }

    if staged && !revs.is_empty() {
        return Err("--staged takes no revisions".into());
    }
    if staged {
        return Ok(Parsed::Spec(Spec::Staged));
    }

    match revs.len() {
        0 => Ok(Parsed::Spec(Spec::Worktree)),
        1 => {
            let arg = &revs[0];
            // `a..b` and `a...b` both mean "compare these two" here; git's
            // three-dot merge-base semantics are not worth the surprise.
            match arg.split_once("..") {
                Some((a, b)) => {
                    let b = b.strip_prefix('.').unwrap_or(b);
                    if a.is_empty() || b.is_empty() {
                        return Err(format!("`{arg}` needs a revision on both sides"));
                    }
                    Ok(Parsed::Spec(Spec::Range(a.to_string(), b.to_string())))
                }
                None => Ok(Parsed::Spec(Spec::Rev(arg.clone()))),
            }
        }
        2 => Ok(Parsed::Spec(Spec::Range(revs[0].clone(), revs[1].clone()))),
        n => Err(format!("expected at most 2 revisions, got {n}")),
    }
}

/// How long a half-typed key sequence waits for the next key, as nvim's
/// `timeoutlen` does. Shorter than nvim's 1000ms, which feels sluggish when
/// `<Space>` alone has to resolve to an action.
const TIMEOUT: Duration = Duration::from_millis(500);

fn run(mut terminal: DefaultTerminal, mut app: App) -> Result<()> {
    let theme = Theme::default();
    loop {
        terminal.draw(|f| ui::draw(f, &mut app, &theme))?;
        if app.quit {
            return Ok(());
        }

        // Only wake on a timer while a sequence is open; otherwise block, so
        // an idle gd costs nothing.
        let effect = if app.pending.is_empty() || event::poll(TIMEOUT)? {
            match event::read()? {
                Event::Key(key) => app.on_key(key),
                // Redrawing on the next iteration is all a resize needs.
                _ => Effect::None,
            }
        } else {
            app.flush_pending()
        };

        if let Effect::OpenEditor { path, line } = effect {
            edit(&mut terminal, &mut app, &path, line)?;
        }
    }
}

/// Hand the terminal to `$EDITOR`, then take it back.
fn edit(terminal: &mut DefaultTerminal, app: &mut App, path: &str, line: usize) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());

    let full = app.repo.root().join(path);
    if !full.exists() {
        app.message = Some(format!("{path} does not exist in the working tree"));
        return Ok(());
    }

    ratatui::restore();

    // `+N` is understood by vi, vim, nvim, emacs and nano alike.
    let status = Command::new(&editor)
        .arg(format!("+{line}"))
        .arg(&full)
        .current_dir(app.repo.root())
        .status();

    *terminal = ratatui::init();
    terminal.clear()?;

    match status {
        Ok(_) => {
            // The file may well have changed; re-read so the panes are honest.
            app.reload();
        }
        Err(e) => app.message = Some(format!("could not run {editor}: {e}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(args: &[&str]) -> Spec {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match parse_args(&owned) {
            Ok(Parsed::Spec(s)) => s,
            _ => panic!("expected a spec for {args:?}"),
        }
    }

    #[test]
    fn no_arguments_means_head_against_the_working_tree() {
        assert_eq!(spec(&[]), Spec::Worktree);
    }

    #[test]
    fn staged_flags_are_equivalent() {
        assert_eq!(spec(&["--staged"]), Spec::Staged);
        assert_eq!(spec(&["--cached"]), Spec::Staged);
        assert_eq!(spec(&["-s"]), Spec::Staged);
    }

    #[test]
    fn one_revision_compares_against_the_working_tree() {
        assert_eq!(spec(&["HEAD~3"]), Spec::Rev("HEAD~3".into()));
    }

    #[test]
    fn range_syntax_is_accepted_in_both_forms() {
        let expected = Spec::Range("v1".into(), "v2".into());
        assert_eq!(spec(&["v1..v2"]), expected);
        assert_eq!(spec(&["v1...v2"]), expected);
        assert_eq!(spec(&["v1", "v2"]), expected);
    }

    #[test]
    fn double_dash_protects_revisions_that_look_like_flags() {
        assert_eq!(spec(&["--", "-weird-branch"]), Spec::Rev("-weird-branch".into()));
    }

    #[test]
    fn bad_input_is_rejected() {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_args(&owned(&["--nope"])).is_err());
        assert!(parse_args(&owned(&["a", "b", "c"])).is_err());
        assert!(parse_args(&owned(&["--staged", "HEAD"])).is_err());
        assert!(parse_args(&owned(&["..v2"])).is_err());
    }

    #[test]
    fn help_and_version_short_circuit() {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(parse_args(&owned(&["-h"])), Ok(Parsed::Help)));
        assert!(matches!(parse_args(&owned(&["--version"])), Ok(Parsed::Version)));
    }
}
