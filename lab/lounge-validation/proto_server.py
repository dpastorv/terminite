#!/usr/bin/env python3
"""Mock terminite proto server — the room's ActivityStore + timing authority.

Listens on a Unix socket, speaks newline-delimited JSON request/response
(matching terminite's "JSON line-delimited" proto). Holds an in-memory
ActivityStore matching guide/activities-design.md §"The model".

This process is the SINGLE timing authority for the lab. Every operation is
logged to proto.log as one JSON line with a perf_counter timestamp. Latency
math (E1, E4) is computed from that log alone, so cross-process clock skew and
MCP/bridge overhead never distort the design's measured latency.

Identity is host-assigned: a connection sends `hello {actor, agent_name,
parent}` once, and every `activity_emit` on that connection is attributed to
that identity. An agent cannot claim to be someone else — this is the
"identity = visible coordinate" claim from the design, enforced at the wire.

Run:
    LOUNGE_SOCK=/tmp/x.sock LOUNGE_PROTO_LOG=/tmp/proto.log \
        python3 proto_server.py [--max-activities N]
"""
import json
import os
import socket
import sys
import threading
import time

# Caps — mirror activities-design.md §"Resource discipline".
DEFAULT_MAX_ACTIVITIES = 10_000
MAX_INPUT_BYTES = 65_536
MAX_OUTPUT_BYTES = 65_536
MAX_ACTIVITY_TAGS = 32

VALID_KINDS = {"tool_call", "agent_message", "user_prompt"}
VALID_STATUS = {"pending", "in_progress", "completed", "failed"}


def _truncate(s, limit):
    if s is None:
        return None
    b = s.encode("utf-8", "replace")
    if len(b) <= limit:
        return s
    return b[:limit].decode("utf-8", "ignore") + f"... [{len(b) - limit} bytes truncated]"


class ActivityStore:
    """In-memory store with the design's caps and oldest-closed-first eviction."""

    def __init__(self, max_activities, logfn):
        self._lock = threading.Lock()
        self._items = {}          # id -> activity dict
        self._next_id = 1
        self._max = max_activities
        self._log = logfn
        self.eviction_count = 0

    def emit(self, conn_identity, params):
        kind = params.get("kind")
        if kind not in VALID_KINDS:
            raise ValueError(f"invalid kind: {kind!r}")
        status = params.get("status")
        if status is None:
            # An emitted (opt-in) action describes something already done.
            status = "completed" if kind == "tool_call" else "completed"
        if status not in VALID_STATUS:
            raise ValueError(f"invalid status: {status!r}")

        tags = list(params.get("tags") or [])[:MAX_ACTIVITY_TAGS]
        now = time.time()
        with self._lock:
            aid = self._next_id
            self._next_id += 1
            act = {
                "id": aid,
                "parent": conn_identity["parent"],     # {"type": "block"|"actor", "ref": "..."}
                "actor": conn_identity["actor"],         # host-assigned, not agent-claimed
                "agent_name": conn_identity["agent_name"],
                "kind": kind,
                "title": params.get("title", ""),
                "input": _truncate(params.get("input"), MAX_INPUT_BYTES),
                "output": _truncate(params.get("output"), MAX_OUTPUT_BYTES),
                "to": params.get("to"),                  # agent_message addressee or None (broadcast)
                "text": _truncate(params.get("text"), MAX_OUTPUT_BYTES),
                "status": status,
                "opened_at": now,
                "closed_at": now if status in ("completed", "failed") else None,
                "tags": tags,
            }
            self._items[aid] = act
            self._evict_if_needed()
        self._log("emit", id=aid, by=conn_identity["actor"], kind=kind,
                  status=status, to=act["to"])
        return {"id": aid, "opened_at": now, "status": status}

    def _evict_if_needed(self):
        # caller holds lock
        if len(self._items) <= self._max:
            return
        # oldest closed first; pinned (tag "pin") survive longer
        closed = sorted(
            (a for a in self._items.values()
             if a["closed_at"] is not None and "pin" not in a["tags"]),
            key=lambda a: (a["closed_at"], a["id"]),
        )
        i = 0
        while len(self._items) > self._max and i < len(closed):
            del self._items[closed[i]["id"]]
            self.eviction_count += 1
            i += 1
        # if still over (all open/pinned), drop oldest overall as last resort
        if len(self._items) > self._max:
            rest = sorted(self._items.values(),
                          key=lambda a: (a["opened_at"], a["id"]))
            j = 0
            while len(self._items) > self._max and j < len(rest):
                aid = rest[j]["id"]
                if "pin" not in self._items[aid]["tags"]:
                    del self._items[aid]
                    self.eviction_count += 1
                j += 1

    def list(self, params, conn_identity):
        actor = params.get("actor")
        parent = params.get("parent")
        kind = params.get("kind")
        to = params.get("to")
        since_id = params.get("since_id", 0)
        with self._lock:
            out = []
            for a in self._items.values():
                if a["id"] <= since_id:
                    continue
                if actor is not None and a["actor"] != actor:
                    continue
                if kind is not None and a["kind"] != kind:
                    continue
                if to is not None and a.get("to") != to:
                    continue
                if parent is not None and (a["parent"] or {}).get("ref") != parent:
                    continue
                out.append(dict(a))
            out.sort(key=lambda a: a["id"])  # time order == id order (monotonic)
        ids = [a["id"] for a in out]
        # log who saw what — this is the read side of the latency measurement
        self._log("list", conn_actor=(conn_identity or {}).get("actor"),
                  ids=ids, filter={"actor": actor, "kind": kind, "to": to,
                                   "since_id": since_id})
        return {"activities": out}

    def get(self, params):
        aid = params.get("id")
        with self._lock:
            a = self._items.get(aid)
            if a is None:
                raise KeyError(f"no activity {aid}")
            return {"activity": dict(a)}

    def tag_add(self, params):
        aid, tag = params.get("id"), params.get("tag")
        with self._lock:
            a = self._items.get(aid)
            if a is None:
                raise KeyError(f"no activity {aid}")
            if tag not in a["tags"] and len(a["tags"]) < MAX_ACTIVITY_TAGS:
                a["tags"].append(tag)
            tags = list(a["tags"])
        self._log("tag_add", id=aid, tag=tag)
        return {"ok": True, "tags": tags}

    def tag_remove(self, params):
        aid, tag = params.get("id"), params.get("tag")
        with self._lock:
            a = self._items.get(aid)
            if a is None:
                raise KeyError(f"no activity {aid}")
            if tag in a["tags"]:
                a["tags"].remove(tag)
            tags = list(a["tags"])
        self._log("tag_remove", id=aid, tag=tag)
        return {"ok": True, "tags": tags}

    def snapshot_stats(self):
        with self._lock:
            per_actor = {}
            for a in self._items.values():
                per_actor[a["actor"]] = per_actor.get(a["actor"], 0) + 1
            return {"total": len(self._items), "per_actor": per_actor,
                    "eviction_count": self.eviction_count, "max": self._max}


class ProtoServer:
    def __init__(self, sock_path, log_path, max_activities):
        self.sock_path = sock_path
        self._logf = open(log_path, "a", buffering=1)
        self._loglock = threading.Lock()
        self.store = ActivityStore(max_activities, self._log)
        self._conn_seq = 0

    def _log(self, op, **fields):
        rec = {"t": time.perf_counter(), "wall": time.time(), "op": op}
        rec.update(fields)
        line = json.dumps(rec)
        with self._loglock:
            self._logf.write(line + "\n")

    def serve(self):
        if os.path.exists(self.sock_path):
            os.unlink(self.sock_path)
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(self.sock_path)
        srv.listen(64)
        self._log("server_up", sock=self.sock_path)
        print(f"proto_server listening on {self.sock_path}", file=sys.stderr, flush=True)
        try:
            while True:
                conn, _ = srv.accept()
                self._conn_seq += 1
                threading.Thread(target=self._handle, args=(conn, self._conn_seq),
                                 daemon=True).start()
        except KeyboardInterrupt:
            pass
        finally:
            srv.close()
            if os.path.exists(self.sock_path):
                os.unlink(self.sock_path)

    def _handle(self, conn, conn_id):
        identity = None  # set by hello
        f = conn.makefile("rwb")
        try:
            for raw in f:
                raw = raw.strip()
                if not raw:
                    continue
                try:
                    req = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                rid = req.get("id")
                method = req.get("method")
                params = req.get("params") or {}
                try:
                    if method == "hello":
                        identity = {
                            "actor": params["actor"],
                            "agent_name": params.get("agent_name", params["actor"]),
                            "parent": params.get("parent")
                                      or {"type": "actor", "ref": params["actor"]},
                        }
                        self._log("hello", conn=conn_id, actor=identity["actor"])
                        result = {"ok": True, "actor": identity["actor"]}
                    elif method == "activity_emit":
                        if identity is None:
                            raise ValueError("emit before hello (no identity)")
                        result = self.store.emit(identity, params)
                    elif method == "activities_list":
                        result = self.store.list(params, identity)
                    elif method == "activity_get":
                        result = self.store.get(params)
                    elif method == "activity_tag_add":
                        result = self.store.tag_add(params)
                    elif method == "activity_tag_remove":
                        result = self.store.tag_remove(params)
                    elif method == "stats":
                        result = self.store.snapshot_stats()
                    else:
                        raise ValueError(f"unknown method {method!r}")
                    resp = {"id": rid, "result": result}
                except Exception as e:  # noqa: BLE001 — report any verb error to caller
                    resp = {"id": rid, "error": {"message": str(e)}}
                f.write((json.dumps(resp) + "\n").encode())
                f.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            f.close()
            conn.close()


def main():
    import argparse
    p = argparse.ArgumentParser()
    p.add_argument("--sock", default=os.environ.get("LOUNGE_SOCK", "/tmp/lounge-validation.sock"))
    p.add_argument("--log", default=os.environ.get("LOUNGE_PROTO_LOG", "/tmp/lounge-proto.log"))
    p.add_argument("--max-activities", type=int,
                   default=int(os.environ.get("LOUNGE_MAX_ACTIVITIES", DEFAULT_MAX_ACTIVITIES)))
    args = p.parse_args()
    ProtoServer(args.sock, args.log, args.max_activities).serve()


if __name__ == "__main__":
    main()
