#!/usr/bin/env bash
# Build gd and link it into your PATH.
#
#   ./install.sh                 # -> ~/.local/bin
#   PREFIX=/usr/local ./install.sh
#   ./install.sh --uninstall
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"

if [ "${1:-}" = "--uninstall" ]; then
    link="$BINDIR/gd"
    [ -L "$link" ] && rm -f "$link" && echo "removed $link"
    exit 0
fi

command -v cargo >/dev/null || { echo "FATAL: cargo is required (https://rustup.rs)"; exit 1; }
command -v git   >/dev/null || { echo "FATAL: git is required"; exit 1; }
command -v rg    >/dev/null || echo "note: ripgrep not found -- <leader>k falls back to a substring scan"

cargo build --release --manifest-path "$HERE/Cargo.toml"

mkdir -p "$BINDIR"
# A symlink, so `git pull && cargo build --release` is the upgrade path.
ln -sfn "$HERE/target/release/gd" "$BINDIR/gd"
echo "installed gd -> $BINDIR/gd"

case ":$PATH:" in
    *":$BINDIR:"*) ;;
    *) echo "warning: $BINDIR is not on your PATH" ;;
esac

if command -v gd >/dev/null && [ "$(command -v gd)" != "$BINDIR/gd" ]; then
    echo "warning: another gd shadows this one at $(command -v gd)"
fi
