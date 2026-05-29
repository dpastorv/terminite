#!/usr/bin/env python3
"""Scripted agent — spawns an MCP bridge and acts in the room.

This is NOT an LLM. It is a deterministic script that exercises the protocol
mechanics: emit, poll, address, reply. It cannot test whether a real agent
would *discover* the tools from their prose (that needs the real-Codex
follow-up named in lab/README §Option A) — but it can prove the wire shape
delivers visibility, attribution, ordering, and addressing, and it lets us
measure pull-poll latency.

Shapes (model the two emission rhythms from activities-design.md):
  --shape codex   high-volume, all visible (ACP-hosted automatic path)
  --shape claude  selective opt-in (shell-hosted; emits only shareworthy acts)

Modes (what this instance does in a scenario):
  emit_tools   emit N tool-call activities, then linger
  poll         poll activities_list for others, log every first-sighting
  converse     emit an agent_message to --address, then poll & reply once
  worksession  (claude shape) run a scripted work session, emit selectively

Identity is passed to the bridge via env, not chosen here.
"""
import argparse
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))

# Realistic tool-call menus per shape.
CODEX_TOOLS = [
    ("read_file", "Read src/renderer.rs"),
    ("bash", "cargo build"),
    ("grep", "grep handle_acp_event"),
    ("edit_file", "Edit src/acp.rs"),
    ("read_file", "Read guide/activities-design.md"),
]
# Claude-shape: of many internal actions, only these few are worth surfacing.
CLAUDE_SHAREWORTHY = [
    ("edit_file", "Edited proto_server.py — added eviction"),
    ("decision", "Chose proto.log as single timing authority"),
    ("bash", "Ran the E1 scenario green"),
]


class MCPAgent:
    """Minimal MCP client over a spawned bridge subprocess."""

    def __init__(self, actor, agent_name, parent, sock):
        env = dict(os.environ)
        env["LOUNGE_ACTOR"] = actor
        env["LOUNGE_AGENT_NAME"] = agent_name
        env["LOUNGE_SOCK"] = sock
        if parent:
            env["LOUNGE_PARENT"] = parent
        self.actor = actor
        self._proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "mcp_bridge.py")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env,
        )
        self._id = 0
        self._lock = threading.Lock()
        self._handshake()

    def _rpc(self, method, params=None, notification=False):
        with self._lock:
            msg = {"jsonrpc": "2.0", "method": method}
            if params is not None:
                msg["params"] = params
            if not notification:
                self._id += 1
                msg["id"] = self._id
            self._proc.stdin.write((json.dumps(msg) + "\n").encode())
            self._proc.stdin.flush()
            if notification:
                return None
            line = self._proc.stdout.readline()
            return json.loads(line)

    def _handshake(self):
        self._rpc("initialize", {"protocolVersion": "2025-06-18",
                                 "capabilities": {}, "clientInfo":
                                 {"name": "agent_mock", "version": "0.1.0"}})
        self._rpc("notifications/initialized", notification=True)

    def call(self, tool, args):
        resp = self._rpc("tools/call", {"name": tool, "arguments": args})
        result = resp.get("result", {})
        text = result.get("content", [{}])[0].get("text", "{}")
        if result.get("isError"):
            raise RuntimeError(text)
        return json.loads(text)

    def emit(self, kind, title, **kw):
        args = {"kind": kind, "title": title}
        args.update({k: v for k, v in kw.items() if v is not None})
        return self.call("terminite_activity_emit", args)

    def list(self, **filters):
        args = {k: v for k, v in filters.items() if v is not None}
        return self.call("terminite_activities_list", args)["activities"]

    def close(self):
        try:
            self._proc.stdin.close()
            self._proc.wait(timeout=3)
        except Exception:  # noqa: BLE001
            self._proc.kill()


# --- modes -----------------------------------------------------------------

def mode_emit_tools(ag, shape, count, interval_ms):
    menu = CODEX_TOOLS if shape == "codex" else CLAUDE_SHAREWORTHY
    for i in range(count):
        tool, title = menu[i % len(menu)]
        ag.emit("tool_call", f"{title} (#{i+1})",
                input=tool, output="ok", status="completed")
        if interval_ms:
            time.sleep(interval_ms / 1000.0)


def mode_poll(ag, duration_ms, poll_ms, out_path):
    """Poll for others' activities; record first-sighting (agent-side perf)."""
    seen = {}                       # id -> agent-side perf_counter of first sighting
    deadline = time.perf_counter() + duration_ms / 1000.0
    since = 0
    polls = 0
    while time.perf_counter() < deadline:
        acts = ag.list(since_id=since)
        polls += 1
        now = time.perf_counter()
        for a in acts:
            if a["actor"] == ag.actor:
                continue  # ignore self
            if a["id"] not in seen:
                seen[a["id"]] = now
            since = max(since, a["id"])
        time.sleep(poll_ms / 1000.0)
    ru = os.times()
    _write(out_path, {
        "role": "poll", "actor": ag.actor,
        "seen_ids": sorted(seen.keys()), "polls": polls,
        "cpu_user": ru.user, "cpu_sys": ru.system,
    })


def mode_converse(ag, address, duration_ms, poll_ms, out_path):
    """Send one addressed message, then watch for replies addressed to me."""
    sent = ag.emit("agent_message", f"hello {address}",
                   to=address, text=f"{ag.actor}: are you there, {address}?")
    received = []
    replied = False
    deadline = time.perf_counter() + duration_ms / 1000.0
    since = 0
    while time.perf_counter() < deadline:
        msgs = ag.list(kind="agent_message", to=ag.actor, since_id=since)
        for m in msgs:
            since = max(since, m["id"])
            if m["actor"] == ag.actor:
                continue
            received.append({"id": m["id"], "from": m["actor"], "text": m["text"]})
            if not replied:
                ag.emit("agent_message", f"reply to {m['actor']}",
                        to=m["actor"], text=f"{ag.actor}: yes, I see {m['actor']}.act-{m['id']}")
                replied = True
        # also confirm broadcasts are distinguishable: a broadcast has to:None
        time.sleep(poll_ms / 1000.0)
    _write(out_path, {"role": "converse", "actor": ag.actor,
                      "sent_id": sent["id"], "received": received})


def mode_worksession(ag, out_path):
    """E5: claude-shape scripted work session. Emit only shareworthy actions.

    The point is to FEEL what fits the three ActivityKinds and what doesn't.
    Script: edit 3 files, run 5 commands, make 1 decision. Of these, choose
    what a peer would actually benefit from seeing.
    """
    log = []

    def share(kind, title, **kw):
        r = ag.emit(kind, title, **kw)
        log.append({"emitted": True, "kind": kind, "title": title, "id": r["id"]})

    def skip(reason):
        log.append({"emitted": False, "reason": reason})

    # edit 3 files — share the ones that change shared surface, skip a trivial one
    share("tool_call", "Edited guide/activities-design.md — added NOTE from E1",
          input="edit_file", output="design doc updated")
    share("tool_call", "Edited src/acp.rs — wired activity emission",
          input="edit_file", output="+18 lines")
    skip("edited a local scratch note — not relevant to the room")
    # run 5 commands — share the meaningful results, skip noise
    skip("ran `ls` — navigation noise")
    skip("ran `git status` — navigation noise")
    share("tool_call", "Ran cargo test — 42 passed",
          input="bash", output="test result: ok. 42 passed; 0 failed")
    skip("ran `cd lab` — navigation noise")
    share("tool_call", "Ran the E1 regression — green",
          input="bash", output="E1 pass")
    # 1 decision — the most important thing to surface, but it's not a tool call
    share("agent_message", "Decision: porting ActivityStore to Rust as-is",
          to=None, text="Decided the design holds; will port ActivityStore "
                        "parallel to BlockStore with the same caps.")
    _write(out_path, {"role": "worksession", "actor": ag.actor, "log": log})


def _write(path, obj):
    if path:
        with open(path, "w") as f:
            json.dump(obj, f, indent=2)
    else:
        json.dump(obj, sys.stdout, indent=2)
        sys.stdout.write("\n")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--actor", required=True)
    p.add_argument("--agent-name")
    p.add_argument("--parent")               # e.g. "actor:codex-1" or "block:B1"
    p.add_argument("--sock", default=os.environ.get("LOUNGE_SOCK", "/tmp/lounge-validation.sock"))
    p.add_argument("--shape", choices=["codex", "claude"], default="codex")
    p.add_argument("--mode", required=True,
                   choices=["emit_tools", "poll", "converse", "worksession"])
    p.add_argument("--count", type=int, default=10)
    p.add_argument("--interval-ms", type=int, default=300)
    p.add_argument("--duration-ms", type=int, default=8000)
    p.add_argument("--poll-ms", type=int, default=250)
    p.add_argument("--address")
    p.add_argument("--out")
    p.add_argument("--start-delay-ms", type=int, default=0)
    args = p.parse_args()

    if args.start_delay_ms:
        time.sleep(args.start_delay_ms / 1000.0)

    ag = MCPAgent(args.actor, args.agent_name or args.actor, args.parent, args.sock)
    try:
        if args.mode == "emit_tools":
            mode_emit_tools(ag, args.shape, args.count, args.interval_ms)
        elif args.mode == "poll":
            mode_poll(ag, args.duration_ms, args.poll_ms, args.out)
        elif args.mode == "converse":
            mode_converse(ag, args.address, args.duration_ms, args.poll_ms, args.out)
        elif args.mode == "worksession":
            mode_worksession(ag, args.out)
    finally:
        ag.close()


if __name__ == "__main__":
    main()
