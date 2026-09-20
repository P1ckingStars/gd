//! Grep backends for the `<leader>k` / `<leader>fw` pickers.
//!
//! When the right-hand pane is the working tree -- the common case -- we drive
//! ripgrep with the same flags as the user's telescope `vimgrep_arguments`, so
//! results match what telescope would show. For revision-to-revision diffs the
//! files may not exist on disk at all, so we fall back to a smart-case
//! substring scan over the blobs we already fetched.

use std::process::Command;

use crate::git::Spec;

/// One grep hit.
#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Cap on results, matching telescope's practical limit. Past this the picker
/// stops being a picker.
pub const MAX_HITS: usize = 2000;

/// argv has a size limit; feed ripgrep paths in batches well under it.
const PATH_BATCH: usize = 256;

/// The flags mirror the user's telescope `vimgrep_arguments`.
fn rg_args() -> Vec<&'static str> {
    vec![
        "--color=never",
        "--no-heading",
        "--with-filename",
        "--line-number",
        "--column",
        "--smart-case",
        "--hidden",
        "--glob",
        "!**/.git/*",
        "--trim",
    ]
}

/// Is ripgrep usable for this comparison?
pub fn uses_ripgrep(spec: &Spec) -> bool {
    matches!(spec, Spec::Worktree | Spec::Rev(_)) && which_rg()
}

fn which_rg() -> bool {
    Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run ripgrep for `query` across `paths`, relative to `root`.
pub fn ripgrep(root: &std::path::Path, query: &str, paths: &[String]) -> Vec<Hit> {
    if query.is_empty() || paths.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for batch in paths.chunks(PATH_BATCH) {
        if hits.len() >= MAX_HITS {
            break;
        }
        let out = Command::new("rg")
            .current_dir(root)
            .args(rg_args())
            .arg("-e")
            .arg(query)
            .arg("--")
            .args(batch)
            .output();
        let Ok(out) = out else { return hits };
        // Exit code 1 just means "no matches"; only 2+ is a real error, and we
        // surface that as an empty result rather than interrupting typing.
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if hits.len() >= MAX_HITS {
                break;
            }
            if let Some(hit) = parse_vimgrep(line) {
                hits.push(hit);
            }
        }
    }
    hits
}

/// Parse one `path:line:col:text` record.
///
/// Splitting from the left would break on a path containing colons, but
/// ripgrep echoes back the repo-relative POSIX paths we hand it, so the first
/// three colons are ours. The column is parsed only to locate the text.
fn parse_vimgrep(line: &str) -> Option<Hit> {
    let (path, rest) = line.split_once(':')?;
    let (lno, rest) = rest.split_once(':')?;
    let (col, text) = rest.split_once(':')?;
    col.parse::<usize>().ok()?;
    Some(Hit {
        path: path.to_string(),
        line: lno.parse().ok()?,
        text: text.to_string(),
    })
}

/// Smart-case substring scan, used when the content is not on disk.
///
/// `sources` is `(path, contents)`.
pub fn in_memory(query: &str, sources: &[(String, String)]) -> Vec<Hit> {
    if query.is_empty() {
        return Vec::new();
    }
    // Smart case: an all-lowercase query ignores case, as in telescope.
    let fold = !query.chars().any(char::is_uppercase);
    let needle = if fold { query.to_lowercase() } else { query.into() };

    let mut hits = Vec::new();
    for (path, content) in sources {
        for (i, line) in content.lines().enumerate() {
            if hits.len() >= MAX_HITS {
                return hits;
            }
            let hay = if fold { line.to_lowercase() } else { line.into() };
            if hay.contains(&needle) {
                hits.push(Hit {
                    path: path.clone(),
                    line: i + 1,
                    text: line.trim_start().to_string(),
                });
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_vimgrep_record() {
        let h = parse_vimgrep("src/main.rs:12:5:    let x = 1;").unwrap();
        assert_eq!(h.path, "src/main.rs");
        assert_eq!(h.line, 12);
        assert_eq!(h.text, "    let x = 1;");
    }

    #[test]
    fn keeps_colons_that_belong_to_the_matched_text() {
        let h = parse_vimgrep("a.rs:1:1:foo: bar: baz").unwrap();
        assert_eq!(h.text, "foo: bar: baz");
    }

    #[test]
    fn rejects_malformed_records() {
        assert!(parse_vimgrep("no colons here").is_none());
        assert!(parse_vimgrep("a.rs:notanumber:1:x").is_none());
        assert!(parse_vimgrep("a.rs:1:notanumber:x").is_none());
    }

    #[test]
    fn in_memory_is_smart_case() {
        let src = vec![("a.rs".to_string(), "Hello world\nhello again\n".to_string())];
        // Lowercase query matches both.
        assert_eq!(in_memory("hello", &src).len(), 2);
        // Uppercase query is case-sensitive.
        let hits = in_memory("Hello", &src);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn in_memory_reports_one_based_line_numbers() {
        let src = vec![("a.rs".to_string(), "xx\nfoo\n".to_string())];
        let hits = in_memory("foo", &src);
        assert_eq!(hits[0].line, 2);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let src = vec![("a.rs".to_string(), "anything\n".to_string())];
        assert!(in_memory("", &src).is_empty());
    }
}
