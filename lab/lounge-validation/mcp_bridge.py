#!/usr/bin/env python3
"""Mock terminite MCP bridge — what each agent spawns to reach the room.

Speaks MCP (JSON-RPC 2.0, newline-delimited over stdio) to the agent, and
relays tool calls to proto_server.py over a Unix socket. The tool catalog and
its prose are copied verbatim from guide/activities-design.md §"Surface" /
§"Emission paths" — that prose is part of the design under test.

Identity is host-assigned, not agent-claimed. The bridge reads its actor label
from the environment (LOUNGE_ACTOR / LOUNGE_AGENT_NAME / LOUNGE_PARENT) and
sends `hello` to the proto on connect. Every emit is attributed to that
identity by the proto, regardless of what the agent passes. This models
terminite knowing which pane/block a session inhabits.

LAB DEVIATION (documented in FINDINGS): this bridge is persistent for the
agent's lifetime rather than spawned per-call. A fresh Python process per call
would tax E4's latency numbers with interpreter startup, which terminite's Rust
bridge does not incur. Latency is measured at the proto layer regardless.
"""
import json
import os
import socket
import sys
import threading

PROTOCOL_VERSION = "2025-06-18"

# Tool catalog — descriptions are the design's prose, under test for clarity.
TOOLS = [
    {
        "name": "terminite_activity_emit",
        "description": (
            "Surface an action you just took so other actors in the room can "
            "see it. Use this for file edits, important decisions, signals to "
            "other agents, or anything you want addressable as B?.act-N. "
            "Calling this is opt-in; you remain otherwise opaque to the room "
            "until you do."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": {"type": "string",
                         "enum": ["tool_call", "agent_message", "user_prompt"]},
                "title": {"type": "string"},
                "to": {"type": "string",
                       "description": "agent_message addressee; omit to broadcast to the room"},
                "text": {"type": "string", "description": "agent_message body"},
                "input": {"type": "string"},
                "output": {"type": "string"},
            },
            "required": ["kind", "title"],
        },
    },
    {
        "name": "terminite_activities_list",
        "description": (
            "What's been happening in the room. Returns agent tool calls, "
            "messages, and human prompts in time order. Filter by actor label "
            "to see one agent's actions; omit to see all."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "actor": {"type": "string"},
                "kind": {"type": "string"},
                "to": {"type": "string"},
                "since_id": {"type": "integer"},
            },
        },
    },
    {
        "name": "terminite_activity_get",
        "description": "Fetch a single activity by its numeric id.",
        "inputSchema": {"type": "object",
                        "properties": {"id": {"type": "integer"}},
                        "required": ["id"]},
    },
    {
        "name": "terminite_activity_tag_add",
        "description": "Attach a tag to an activity. Tags share one namespace with block tags.",
        "inputSchema": {"type": "object",
                        "properties": {"id": {"type": "integer"}, "tag": {"type": "string"}},
                        "required": ["id", "tag"]},
    },
    {
        "name": "terminite_activity_tag_remove",
        "description": "Remove a tag from an activity.",
        "inputSchema": {"type": "object",
                        "properties": {"id": {"type": "integer"}, "tag": {"type": "string"}},
                        "required": ["id", "tag"]},
    },
]

TOOL_TO_VERB = {
    "terminite_activity_emit": "activity_emit",
    "terminite_activities_list": "activities_list",
    "terminite_activity_get": "activity_get",
    "terminite_activity_tag_add": "activity_tag_add",
    "terminite_activity_tag_remove": "activity_tag_remove",
}


class ProtoClient:
    """Persistent connection to the proto socket. Serializes requests."""

    def __init__(self, sock_path, identity):
        self._lock = threading.Lock()
        self._id = 0
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.connect(sock_path)
        self._f = self._sock.makefile("rwb")
        self.call("hello", identity)

    def call(self, method, params):
        with self._lock:
            self._id += 1
            req = {"id": self._id, "method": method, "params": params}
            self._f.write((json.dumps(req) + "\n").encode())
            self._f.flush()
            line = self._f.readline()
            resp = json.loads(line)
            if "error" in resp:
                raise RuntimeError(resp["error"].get("message", "proto error"))
            return resp["result"]


def main():
    sock_path = os.environ.get("LOUNGE_SOCK", "/tmp/lounge-validation.sock")
    identity = {
        "actor": os.environ["LOUNGE_ACTOR"],
        "agent_name": os.environ.get("LOUNGE_AGENT_NAME", os.environ["LOUNGE_ACTOR"]),
    }
    parent_ref = os.environ.get("LOUNGE_PARENT")
    if parent_ref:
        # "block:B1" or "actor:codex-1"
        kind, _, ref = parent_ref.partition(":")
        identity["parent"] = {"type": kind, "ref": ref}

    proto = ProtoClient(sock_path, identity)

    out = sys.stdout.buffer
    out_lock = threading.Lock()

    def send(msg):
        with out_lock:
            out.write((json.dumps(msg) + "\n").encode())
            out.flush()

    for raw in sys.stdin.buffer:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            continue
        mid = msg.get("id")
        method = msg.get("method")

        if method == "initialize":
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "lounge-validation-bridge", "version": "0.1.0"},
            }})
        elif method == "notifications/initialized":
            pass  # notification, no response
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}})
        elif method == "tools/call":
            params = msg.get("params") or {}
            name = params.get("name")
            args = params.get("arguments") or {}
            verb = TOOL_TO_VERB.get(name)
            if verb is None:
                send({"jsonrpc": "2.0", "id": mid, "error":
                      {"code": -32601, "message": f"unknown tool {name}"}})
                continue
            try:
                result = proto.call(verb, args)
                send({"jsonrpc": "2.0", "id": mid, "result": {
                    "content": [{"type": "text", "text": json.dumps(result)}],
                    "isError": False,
                }})
            except Exception as e:  # noqa: BLE001 — surface as MCP tool error
                send({"jsonrpc": "2.0", "id": mid, "result": {
                    "content": [{"type": "text", "text": str(e)}],
                    "isError": True,
                }})
        else:
            if mid is not None:
                send({"jsonrpc": "2.0", "id": mid, "error":
                      {"code": -32601, "message": f"unknown method {method}"}})


if __name__ == "__main__":
    main()
