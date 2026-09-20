# gd

Read a git diff side by side in the terminal, with your Neovim keys.

```
$ gd
```

```
╭ changed (5) ────────╮╭ HEAD ──────────────────────────────────── working tree ╮
│▾ docs 1             ││  1  fn main() {               │  1  fn main() {         │
│└ D readme.md        ││  2 -    let total = 1;        │  2 +    let total = 42; │
│▾ src 4              ││  3 -    println!("hello");    │  3 +    println!("bye");│
│├ ▾ ui 1             ││  4      for i in 0..10 {      │  4      for i in 0..10 {│
││ └ M split.rs       ││  5          println!("{i}");  │  5          println!("{i│
│├ M filler.rs        ││  6      }                     │  6      }               │
│├ M main.rs          ││                               │  7 +    println!("new");│
│└ ? untracked.rs     ││  7  }                         │  8  }                   │
╰─────────────────────╯╰ src/main.rs ───────────────────────────────── +3 -2 ────╯
 DIFF  master │ src/main.rs │ 4/9 │ +3 -2 │ hunk 1/1 │ ? help
```

Old on the left, new on the right, always on the same row. Only the words that
actually changed are highlighted, not the whole line.

## Why this exists

`git diff` is unified: deletions and insertions are stacked, and you rebuild the
before-and-after in your head. That is fine for three lines and tiring for
thirty. Side-by-side viewers exist, but they are either a web page, or a pager
with its own key bindings you have to learn on top of the ones in your fingers.

`gd` is the diff pane plus the three things you actually do with a diff: find a
file, find a word, walk the directory of changes. Those are bound to the keys
you already use in Neovim.

## Install

```
git clone https://github.com/P1ckingStars/gd
cd gd
./install.sh          # builds and links gd into ~/.local/bin
```

Needs `cargo` and `git`. [ripgrep](https://github.com/BurntSushi/ripgrep) is
optional but recommended -- it is what `<leader>k` searches with.

Note that `gd` is a common shell alias for `git diff`. Check with
`type gd`, and drop the alias if you want this one to win.

## Usage

```
gd                    HEAD against the working tree
gd --staged           HEAD against the index
```

Recent commits are addressed by how far back they are. `1` is the last commit,
`2` the one before it, and so on:

```
gd 1                  the last commit
gd 3                  the last 3 commits, as one diff
gd 2..4               the second, third and fourth commits back
gd 4..2               the same window; either order reads alike
```

A bare `gd N` is exactly `gd 1..N`, so the two spellings never disagree. The
pane titles always show the revisions the numbers resolved to -- `gd 2..4` is
headed `HEAD~4` and `HEAD~1` -- so you can check the arithmetic at a glance.

Revisions work as they always did:

```
gd HEAD~3             a revision against the working tree
gd v1.0..v2.0         two revisions
gd main feature       the same, spelled with a space
```

An argument made only of digits is always a count, never a ref. If you have a
branch or a short SHA spelled with digits alone, write it out -- `gd refs/tags/2`
or `gd 1234^{commit}` -- and gd will treat it as a revision.

## Keys

The bindings come from `~/.config/nvim/lua/keymaps.lua`, so leader is `<Space>`
and the pickers sit where telescope does.

### Finding things -- telescope

| Key | |
|---|---|
| `<leader>j` | find a changed file |
| `<leader>k` | live grep across the changed files |
| `<leader>fw` | grep the word under the cursor |
| `<leader>fs` | find a line in this diff |
| `<leader>fb` | recently opened files |
| `<leader>fr` | resume the last picker |

Inside a picker: `<C-j>`/`<C-k>` (or `<C-n>`/`<C-p>`) move, `<CR>` opens, `<C-u>`
clears the prompt, `<C-w>` deletes a word, `<Esc>` closes. `<C-q>` sends the
whole result set to a quickfix list you then walk with `]q` and `[q`.

### Walking the changes -- neo-tree

| Key | |
|---|---|
| `<leader>n` or `\` | toggle the file tree |
| `]g` / `[g` | next / previous changed file |
| `<CR>` or `l` | open, or expand a directory |
| `<Space>` | expand or collapse |
| `C` or `h` | collapse this directory |
| `z` / `Z` | collapse / expand everything |
| `<BS>` | go to the parent directory |
| `/` | filter the tree |
| `<C-x>` | clear the filter |
| `<C-w>h` / `<C-w>l` | move between the tree and the diff |
| `<C-w><` / `<C-w>>` | narrow / widen the tree |

### Reading the diff

| Key | |
|---|---|
| `j` `k` `<C-d>` `<C-u>` `<C-f>` `<C-b>` `gg` `G` | move, as in nvim |
| `h` `l` `0` | scroll long lines sideways |
| `zz` | centre the cursor |
| `]c` / `[c` | next / previous hunk |
| `/` `n` `N` | search inside this diff |
| `za` | fold open: show the whole file instead of hunks |
| `<Tab>` | switch between side-by-side and unified |
| `s` | swap which version is on the left |
| `e` or `<CR>` | open the file in `$EDITOR` at this line |
| `Y` | copy the path |
| `R` | re-read the repository |
| `?` | every binding, in one screen |
| `q` | quit |

Multi-key sequences resolve the way nvim's do. `<Space>` in the tree both
toggles a node and starts a leader sequence: press it and `gd` waits 500ms for
the next key, exactly like `nowait = false`.

## How it works

- **git** is the real one, shelled out to. That is one process per query, and in
  exchange every `diff.*` setting, rename threshold and pathspec rule you have
  configured keeps working. No libgit2, so no C in the build.
- **Alignment** is the whole design. Both panes render the same slice of one row
  list, and a row that exists on only one side carries a blank on the other.
  There is one scroll offset, so the sides cannot drift.
- **Word-level diff** runs only on lines paired as a modification, and is
  suppressed when the two lines are less than 30% similar -- past that point the
  highlight is confetti rather than information.
- **Grep** uses ripgrep with the same flags as your telescope
  `vimgrep_arguments`. Comparing two revisions, the files may not be on disk at
  all, so it falls back to scanning the blobs it already fetched.

## Development

```
cargo test        # 74 tests, no repository or terminal required
cargo run -- --staged
```

The pieces: `git.rs` talks to git, `diff.rs` turns two texts into aligned rows,
`tree.rs` is the file tree, `keys.rs` is the nvim-style sequence resolver,
`picker.rs` is telescope, `ui.rs` draws, `app.rs` holds it together.

## License

MIT
