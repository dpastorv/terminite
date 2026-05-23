#!/usr/bin/env bash
# Phase 2 Bundle 1 end-to-end proof.
# Run this in a terminite pane (not via Claude's Bash tool — different pty).
# Expect: a small "B1" label appears in the LEFT gutter of this pane,
# anchored to the row where "hello world" is printed.

esc=$'\e'
bel=$'\a'

printf '%s]133;A%s' "$esc" "$bel"      # A: prompt start
printf 'demo$ '
printf '%s]133;B%s' "$esc" "$bel"      # B: prompt end / command start
printf 'echo hello world\n'
printf '%s]133;C%s' "$esc" "$bel"      # C: output start
echo "hello world"
printf '%s]133;D;0%s' "$esc" "$bel"    # D: output end, exit=0

echo
echo "[demo done — look for 'B1' in the left gutter]"
