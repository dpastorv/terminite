#!/usr/bin/env bash
# Real-Codex discoverability follow-up — the regression test from
# guide/activities-design.md §"What success looks like", run against the mock.
#
# Scripted mocks (run.sh) proved the MECHANICS. This proves the load-bearing
# claim a script cannot: "the vocabulary has to be self-evident at the protocol
# layer" (lounge-thesis). We drop a REAL Codex into the mock room as codex-2,
# seed a peer (codex-1) with some activities, and ask — WITHOUT naming any
# tool — "who else is here, what have they done?" Then we check two things:
#   (1) discovery: did Codex call terminite_activities_list unprompted? (proto.log)
#   (2) report:    did its answer correctly name codex-1 and its actions?
#
# Identity is host-assigned: the bridge's env sets LOUNGE_ACTOR=codex-2, so
# Codex cannot claim to be anyone else — same property E1 validated.
set -euo pipefail
cd "$(dirname "$0")"
HERE="$(pwd)"

STAMP="$(date +%Y%m%d-%H%M%S)"
RUNDIR="$HERE/runs/real-codex-$STAMP"
mkdir -p "$RUNDIR"
SOCK="/tmp/lounge-realcodex-$$.sock"
LOG="$RUNDIR/proto.log"
BRIDGE="$HERE/mcp_bridge.py"
# Empty working dir so Codex CANNOT cheat by reading the lab's own source
# (the first run got the right answer by `sed`-ing agent_mock.py — a false
# positive). With an empty cwd, the ONLY channel about codex-1 is the room.
WORKDIR="$(mktemp -d)"

cleanup() {
  [[ -n "${PROTO_PID:-}" ]] && kill "$PROTO_PID" 2>/dev/null || true
  rm -f "$SOCK"
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

echo "== booting mock room (proto_server) =="
LOUNGE_SOCK="$SOCK" LOUNGE_PROTO_LOG="$LOG" python3 proto_server.py &
PROTO_PID=$!
for _ in $(seq 1 100); do [[ -S "$SOCK" ]] && break; sleep 0.05; done
[[ -S "$SOCK" ]] || { echo "proto did not come up"; exit 1; }

echo "== seeding peer codex-1 with 6 tool-call activities =="
python3 agent_mock.py --actor codex-1 --parent actor:codex-1 \
  --shape codex --mode emit_tools --count 6 --interval-ms 50 --sock "$SOCK"

echo "== peer seeded. proto.log so far: =="
grep -c '"op": "emit"' "$LOG" || true

PROMPT='You are one participant ("codex-2") in a shared terminite workspace. \
Another participant may be present in the same room. Without my telling you \
how, find out whether anyone else is here and, if so, what they have been \
doing. Then give me a short report: who (if anyone) you found, and what they \
did. If you find no one, say so plainly.'

echo "== running REAL codex as codex-2 (tools NOT named; empty cwd; bypass) =="
# --dangerously-bypass-approvals-and-sandbox: codex exec gates MCP/dynamic tools
# behind default_tools_approval_mode and cancels them non-interactively
# (approval_policy and -s sandbox changes do NOT unblock it). The bypass is the
# only known switch. Run with the USER'S EXPLICIT AUTHORIZATION (2026-05-29):
# controlled lab, our own bridge touching only a local socket, empty cwd so
# Codex cannot cheat by reading lab source, success judged ONLY by proto.log.
set +e
codex exec \
  --dangerously-bypass-approvals-and-sandbox \
  --skip-git-repo-check \
  -C "$WORKDIR" \
  -c "mcp_servers.terminite.command=\"python3\"" \
  -c "mcp_servers.terminite.args=[\"$BRIDGE\"]" \
  -c "mcp_servers.terminite.env={ LOUNGE_ACTOR = \"codex-2\", LOUNGE_AGENT_NAME = \"Codex\", LOUNGE_SOCK = \"$SOCK\", LOUNGE_PARENT = \"actor:codex-2\" }" \
  "$PROMPT" 2>&1 | tee "$RUNDIR/codex-output.txt"
CODEX_RC=${PIPESTATUS[0]}
set -e

echo ""
echo "================ ANALYSIS ================"
echo "codex exit code: $CODEX_RC"
LIST_CALLS=$(grep '"op": "list"' "$LOG" | grep -c '"conn_actor": "codex-2"' || true)
echo "  list calls by codex-2 (ground truth it used the ROOM): $LIST_CALLS"
echo "--- hellos logged ---"
grep '"op": "hello"' "$LOG" || echo "  (none)"
echo "--- last list ops by codex-2 (what the room returned) ---"
grep '"op": "list"' "$LOG" | grep '"conn_actor": "codex-2"' | tail -3 || echo "  (none)"
echo "=========================================="
echo "artifacts in: $RUNDIR"
