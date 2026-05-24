#!/usr/bin/env bash
# Install the example "Hello" module into ~/.terminite/modules/hello/
# so it shows up in every pane's dropdown.

set -euo pipefail

src="$(cd "$(dirname "$0")" && pwd)/example-module"
dst_root="${TERMINITE_MODULES_DIR:-$HOME/.terminite/modules}"
dst="$dst_root/hello"

mkdir -p "$dst_root"
rm -rf "$dst"
cp -r "$src" "$dst"
chmod +x "$dst/hello.py"

echo "installed 'hello' module → $dst"
echo
echo "next:"
echo "  1) relaunch terminite (cargo run)"
echo "  2) in any pane, click 'Shell ▾' → pick 'Hello'"
echo "  3) type — your keystrokes echo back in the pane"
