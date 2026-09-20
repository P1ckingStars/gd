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

use std::io::IsTerminal;
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
    gd [OPTIONS] <N>
    gd [OPTIONS] <A>..<B>

ARGS:
    <N>                 The last N commits. `gd 1` is the last commit.
    <A>..<B>            Commits A through B, counting back from the newest.
                        `gd 2..4` is the second, third and fourth back.
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

    // Without this, ratatui's init panics with a bare OS error when gd is
    // piped, redirected, or run from a script.
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("gd needs an interactive terminal, and stdout is not one");
    }

    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let repo = Repo::discover(&cwd)?;
    repo.verify(&spec)?;
    let app = App::new(repo, spec)?;

    let terminal = ratatui::init();
    let result = run(terminal, app);
    ratatui::restore();
    result
}

#[derive(Debug)]
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
            match split_range(arg)? {
                Some((a, b)) => range(&a, &b),
                // A bare count is the range starting at the last commit, so
                // `gd 3` and `gd 1..3` mean the same thing.
                None if is_count(arg) => range("1", arg),
                None => Ok(Parsed::Spec(Spec::Rev(arg.clone()))),
            }
        }
        2 => range(&revs[0], &revs[1]),
        n => Err(format!("expected at most 2 revisions, got {n}")),
    }
}

/// Split `a..b` or `a...b`. git's three-dot merge-base semantics are not worth
/// the surprise here, so both spellings mean "compare these two".
fn split_range(arg: &str) -> Result<Option<(String, String)>, String> {
    let Some((a, rest)) = arg.split_once("..") else {
        return Ok(None);
    };
    let b = rest.strip_prefix('.').unwrap_or(rest);
    if a.is_empty() || b.is_empty() {
        return Err(format!("`{arg}` needs a revision on both sides"));
    }
    Ok(Some((a.to_string(), b.to_string())))
}

/// An all-digit argument is a commit count, not a revision name.
fn is_count(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The revision `n` commits back from HEAD.
fn back(n: usize) -> String {
    if n == 0 {
        "HEAD".into()
    } else {
        format!("HEAD~{n}")
    }
}

/// Build a range from two arguments, which are either both counts or both
/// revisions. Counting back from the newest commit, `1` is the last commit, so
/// `2..4` covers the second, third and fourth commits back.
fn range(a: &str, b: &str) -> Result<Parsed, String> {
    let spec = match (is_count(a), is_count(b)) {
        (true, true) => {
            let (x, y): (usize, usize) = match (a.parse(), b.parse()) {
                (Ok(x), Ok(y)) => (x, y),
                _ => return Err(format!("`{a}..{b}` is too large to be a commit count")),
            };
            if x == 0 || y == 0 {
                return Err("commit counts start at 1 -- `gd 1` is the last commit".into());
            }
            // Either order describes the same window of commits.
            let (first, last) = (x.min(y), x.max(y));
            // Everything from the commit before the oldest one named, up to the
            // newest one named.
            Spec::Range(back(last), back(first - 1))
        }
        (false, false) => Spec::Range(a.to_string(), b.to_string()),
        _ => {
            return Err(format!(
                "`{a}..{b}` mixes a commit count with a revision -- spell both out, \
                 as in `HEAD~4..main`"
            ))
        }
    };
    Ok(Parsed::Spec(spec))
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

    fn err(args: &[&str]) -> String {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args(&owned).expect_err(&format!("expected an error for {args:?}"))
    }

    #[test]
    fn a_bare_count_is_the_last_n_commits() {
        assert_eq!(spec(&["1"]), Spec::Range("HEAD~1".into(), "HEAD".into()));
        assert_eq!(spec(&["2"]), Spec::Range("HEAD~2".into(), "HEAD".into()));
        assert_eq!(spec(&["10"]), Spec::Range("HEAD~10".into(), "HEAD".into()));
    }

    #[test]
    fn a_count_range_covers_both_endpoints() {
        // Counting back, 1 is the last commit, so 2..4 is the second, third
        // and fourth back: from the commit before the fourth, up to the second.
        assert_eq!(spec(&["2..4"]), Spec::Range("HEAD~4".into(), "HEAD~1".into()));
        assert_eq!(spec(&["1..3"]), Spec::Range("HEAD~3".into(), "HEAD".into()));
    }

    #[test]
    fn a_bare_count_equals_the_range_that_starts_at_one() {
        // This is the property that makes the shorthand coherent.
        for n in 1..8 {
            assert_eq!(spec(&[&n.to_string()]), spec(&[&format!("1..{n}")]));
        }
    }

    #[test]
    fn count_ranges_read_the_same_in_either_order() {
        assert_eq!(spec(&["4..2"]), spec(&["2..4"]));
        assert_eq!(spec(&["2", "4"]), spec(&["2..4"]));
        assert_eq!(spec(&["2...4"]), spec(&["2..4"]));
    }

    #[test]
    fn counts_are_rejected_when_they_cannot_mean_a_commit() {
        assert!(err(&["0"]).contains("start at 1"));
        assert!(err(&["0..3"]).contains("start at 1"));
        // A count on one side and a revision on the other is ambiguous.
        assert!(err(&["2..main"]).contains("mixes a commit count"));
        assert!(err(&["main..2"]).contains("mixes a commit count"));
    }

    #[test]
    fn revisions_that_merely_look_numeric_still_parse_as_counts() {
        // Documented behaviour: an all-digit argument is always a count, so a
        // ref spelled with digits alone has to be written out in full.
        assert_eq!(spec(&["1234"]), Spec::Range("HEAD~1234".into(), "HEAD".into()));
        assert_eq!(spec(&["refs/tags/2"]), Spec::Rev("refs/tags/2".into()));
        assert_eq!(spec(&["v2"]), Spec::Rev("v2".into()));
        // A SHA with any letter in it is unambiguous already.
        assert_eq!(spec(&["1234abc"]), Spec::Rev("1234abc".into()));
    }

    #[test]
    fn named_revisions_are_untouched_by_the_shorthand() {
        assert_eq!(spec(&["HEAD~3"]), Spec::Rev("HEAD~3".into()));
        assert_eq!(
            spec(&["v1.0..v2.0"]),
            Spec::Range("v1.0".into(), "v2.0".into())
        );
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
