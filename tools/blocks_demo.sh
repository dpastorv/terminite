#!/usr/bin/env bash
# Phase 2 demo — produces a varied set of blocks so you can exercise
# the protocol surface without repeating the same command. Run in a
# terminite pane (NOT via Claude's Bash tool — different pty).
#
# Suggested loop:
#   1) ./tools/blocks_demo.sh             # creates 6 distinct blocks
#   2) terminite blocks 0                 # see them listed
#   3) terminite block 0 <id>             # fetch one block's command + output
#   4) terminite tag 0 <id> something     # tag a block (label brightens a bit)
#   5) terminite cursor 0 <id>            # AI cursor lands there (label goes warm)
#   6) terminite watch                    # live event stream — re-run this script
#                                         # in another pane to see events flow

set -u

esc=$'\e'
bel=$'\a'

# Emit one synthetic block: A → command text → B → C → output lines → D.
#
# Usage: emit_block <command-string> <exit-code> [output-line ...]
# Each output line gets its own '\n' so multi-line outputs span multiple
# rows (the block's output_start..output_end range will reflect that).
emit_block() {
    local cmd="$1"
    local exit="${2:-0}"
    shift 2

    printf '%s]133;A%s' "$esc" "$bel"      # A: prompt start
    printf 'demo$ '
    printf '%s]133;B%s' "$esc" "$bel"      # B: prompt end / command start
    printf '%s\n' "$cmd"
    printf '%s]133;C%s' "$esc" "$bel"      # C: output start
    for line in "$@"; do
        printf '%s\n' "$line"
    done
    printf '%s]133;D;%d%s' "$esc" "$exit" "$bel"  # D: finished
}

emit_block "ls"          0  "Cargo.toml  README.md  src  target  vendor"
emit_block "git status"  0  "On branch main" \
                            "Your branch is up to date with 'origin/main'." \
                            "" \
                            "nothing to commit, working tree clean"
emit_block "cargo build" 0  "    Finished dev profile in 0.42s"
emit_block "false"       1  # no output — exits non-zero
emit_block "echo hi"     0  "hi"
emit_block "cat /nope"   1  "cat: /nope: No such file or directory"

echo
echo "[demo done — 6 new blocks emitted with varied commands, outputs, exit codes."
echo " try (replace <id> with what you saw in the gutter):"
echo "   terminite blocks 0                — list them"
echo "   terminite block 0 <id>            — see command + output, structured"
echo "   terminite tag 0 <id> probe        — gutter label brightens"
echo "   terminite cursor 0 <id>           — label goes warm-yellow"
echo "   terminite cursor-clear 0          — cursor goes away"
echo "   terminite watch                   — live events; re-run this script"
echo "                                       in another pane to see them flow"
echo "]"
