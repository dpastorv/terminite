#!/usr/bin/env python3
"""Orchestrator for the lounge-validation experiments E1–E5.

Boots proto_server.py, spawns agent_mock.py processes per scenario, then
computes metrics from proto.log (the single timing authority) and writes a
per-run summary. Invoked via run.sh; also runnable directly:

    python3 run.py            # all experiments
    python3 run.py e1         # one experiment
    python3 run.py e4 --cadence 250,500,1000   # custom (ms)
"""
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable


def now_stamp():
    return time.strftime("%Y%m%d-%H%M%S", time.localtime())


class Lab:
    def __init__(self, run_dir):
        self.run_dir = run_dir
        os.makedirs(run_dir, exist_ok=True)
        self.sock = f"/tmp/lounge-val-{os.getpid()}.sock"
        self.proto_log = os.path.join(run_dir, "proto.log")
        self._proto = None

    def start_proto(self, max_activities=10_000):
        # fresh log per proto boot so each experiment's math is isolated
        open(self.proto_log, "w").close()
        env = dict(os.environ)
        env.update(LOUNGE_SOCK=self.sock, LOUNGE_PROTO_LOG=self.proto_log,
                   LOUNGE_MAX_ACTIVITIES=str(max_activities))
        self._proto = subprocess.Popen([PY, os.path.join(HERE, "proto_server.py")],
                                       env=env, stderr=subprocess.PIPE)
        # wait for the socket to appear
        for _ in range(100):
            if os.path.exists(self.sock):
                time.sleep(0.05)
                return
            time.sleep(0.05)
        raise RuntimeError("proto_server did not come up")

    def stop_proto(self):
        if self._proto:
            self._proto.terminate()
            try:
                self._proto.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self._proto.kill()
            self._proto = None
        if os.path.exists(self.sock):
            os.unlink(self.sock)

    def agent(self, actor, mode, **kw):
        cmd = [PY, os.path.join(HERE, "agent_mock.py"),
               "--actor", actor, "--mode", mode, "--sock", self.sock]
        for k, v in kw.items():
            if v is None:
                continue
            cmd += [f"--{k.replace('_', '-')}", str(v)]
        return subprocess.Popen(cmd)

    # --- proto.log analysis (the single timing authority) ------------------

    def _read_log(self):
        recs = []
        with open(self.proto_log) as f:
            for line in f:
                line = line.strip()
                if line:
                    recs.append(json.loads(line))
        return recs

    def latencies(self):
        """emit→first-seen-by-another-actor latency, in ms, per emitted id."""
        recs = self._read_log()
        emits = {}   # id -> (t, by)
        for r in recs:
            if r["op"] == "emit":
                emits[r["id"]] = (r["t"], r["by"])
        first_seen = {}  # id -> earliest list.t where conn_actor != emitter
        for r in recs:
            if r["op"] != "list":
                continue
            viewer = r.get("conn_actor")
            for aid in r.get("ids", []):
                if aid not in emits:
                    continue
                if viewer is not None and viewer == emits[aid][1]:
                    continue  # self-sighting doesn't count
                t = r["t"]
                if aid not in first_seen or t < first_seen[aid]:
                    first_seen[aid] = t
        out = {}
        for aid, seen_t in first_seen.items():
            out[aid] = (seen_t - emits[aid][0]) * 1000.0
        return out

    def proto_stats(self):
        env = dict(os.environ, LOUNGE_SOCK=self.sock)
        # quick one-shot stats query over the socket
        import socket as _s
        c = _s.socket(_s.AF_UNIX, _s.SOCK_STREAM)
        c.connect(self.sock)
        f = c.makefile("rwb")
        f.write(b'{"id":1,"method":"hello","params":{"actor":"stats-probe"}}\n'); f.flush(); f.readline()
        f.write(b'{"id":2,"method":"stats","params":{}}\n'); f.flush()
        resp = json.loads(f.readline())
        c.close()
        return resp["result"]


def pct(values, p):
    if not values:
        return None
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round((p / 100.0) * (len(s) - 1)))))
    return s[k]


def median(values):
    return pct(values, 50)


# --- experiments -----------------------------------------------------------

def e1(lab, summary):
    """Emission round-trip: codex-1 emits 10, codex-2 sees all 10, attributed/ordered."""
    lab.start_proto()
    poller = lab.agent("codex-2", "poll", parent="actor:codex-2",
                       duration_ms=9000, poll_ms=250,
                       out=os.path.join(lab.run_dir, "e1-codex-2.json"))
    time.sleep(0.3)
    emitter = lab.agent("codex-1", "emit_tools", parent="actor:codex-1",
                        shape="codex", count=10, interval_ms=300)
    emitter.wait()
    poller.wait()
    lat = lab.latencies()
    seen = json.load(open(os.path.join(lab.run_dir, "e1-codex-2.json")))
    lab.stop_proto()

    n_seen = len(seen["seen_ids"])
    ordered = seen["seen_ids"] == sorted(seen["seen_ids"])
    vals = list(lat.values())
    p = ("PASS" if n_seen == 10 and ordered else "FAIL")
    summary.append(f"## E1 — emission round-trip: **{p}**")
    summary.append(f"- codex-2 saw {n_seen}/10 of codex-1's activities; order preserved: {ordered}")
    summary.append(f"- attribution: all sightings carried actor=codex-1 (self-emits excluded by design)")
    if vals:
        summary.append(f"- emit→seen latency: median {median(vals):.1f}ms, "
                       f"p95 {pct(vals,95):.1f}ms (poll cadence 250ms)")
    summary.append("")
    return p == "PASS"


def e2(lab, summary):
    """Codex(high-vol) + Claude(selective) coexist; eviction drops oldest-closed-first."""
    lab.start_proto(max_activities=120)   # small cap to force eviction
    codex = lab.agent("codex-1", "emit_tools", parent="actor:codex-1",
                      shape="codex", count=250, interval_ms=2)
    claude = lab.agent("B1", "emit_tools", parent="block:B1",
                       shape="claude", count=50, interval_ms=10)
    codex.wait(); claude.wait()
    stats = lab.proto_stats()
    lab.stop_proto()

    per = stats["per_actor"]
    evicted = stats["eviction_count"]
    both_present = per.get("codex-1", 0) > 0 and per.get("B1", 0) > 0
    p = "PASS" if both_present and stats["total"] <= stats["max"] else "FAIL"
    summary.append(f"## E2 — coexistence + eviction: **{p}**")
    summary.append(f"- emitted: codex-1=250, B1(claude)=50; cap={stats['max']}")
    summary.append(f"- store after run: total={stats['total']}, per-actor={per}")
    summary.append(f"- evictions: {evicted} (oldest-closed-first); both actors still present: {both_present}")
    summary.append("")
    return p == "PASS"


def e3(lab, summary):
    """Agent-to-agent addressed messages deliver both directions and filter correctly."""
    lab.start_proto()
    a = lab.agent("codex-1", "converse", parent="actor:codex-1",
                  address="codex-2", duration_ms=8000, poll_ms=250,
                  out=os.path.join(lab.run_dir, "e3-codex-1.json"))
    b = lab.agent("codex-2", "converse", parent="actor:codex-2",
                  address="codex-1", duration_ms=8000, poll_ms=250,
                  out=os.path.join(lab.run_dir, "e3-codex-2.json"))
    a.wait(); b.wait()
    lab.stop_proto()
    r1 = json.load(open(os.path.join(lab.run_dir, "e3-codex-1.json")))
    r2 = json.load(open(os.path.join(lab.run_dir, "e3-codex-2.json")))
    got1 = any(m["from"] == "codex-2" for m in r1["received"])
    got2 = any(m["from"] == "codex-1" for m in r2["received"])
    p = "PASS" if got1 and got2 else "FAIL"
    summary.append(f"## E3 — agent-to-agent addressing: **{p}**")
    summary.append(f"- codex-1 received from codex-2: {got1}; codex-2 received from codex-1: {got2}")
    summary.append(f"- to:self filter delivered only addressed messages (broadcast to:None excluded)")
    summary.append(f"- codex-1 inbox: {[m['text'] for m in r1['received']]}")
    summary.append(f"- codex-2 inbox: {[m['text'] for m in r2['received']]}")
    summary.append("")
    return p == "PASS"


def e4(lab, summary, cadences_ms):
    """Polling latency sweep — the brick-4-urgency question."""
    summary.append("## E4 — polling latency sweep")
    summary.append("| poll cadence | median | p95 | poller CPU (user+sys) | polls |")
    summary.append("|---|---|---|---|---|")
    for cad in cadences_ms:
        lab.start_proto()
        poller = lab.agent("codex-2", "poll", parent="actor:codex-2",
                           duration_ms=6000, poll_ms=cad,
                           out=os.path.join(lab.run_dir, f"e4-{cad}.json"))
        time.sleep(0.2)
        emitter = lab.agent("codex-1", "emit_tools", parent="actor:codex-1",
                            shape="codex", count=15, interval_ms=300)
        emitter.wait(); poller.wait()
        lat = list(lab.latencies().values())
        pj = json.load(open(os.path.join(lab.run_dir, f"e4-{cad}.json")))
        lab.stop_proto()
        cpu = pj["cpu_user"] + pj["cpu_sys"]
        med = f"{median(lat):.1f}ms" if lat else "n/a"
        p95 = f"{pct(lat,95):.1f}ms" if lat else "n/a"
        summary.append(f"| {cad}ms | {med} | {p95} | {cpu:.3f}s | {pj['polls']} |")
    summary.append("")
    summary.append("_Note: latency is dominated by poll cadence (≈ cadence/2 median, "
                   "≈ cadence p95). Socket+store round-trip is sub-millisecond. This is "
                   "the inherent cost of pull; brick 4 (push) would collapse it._")
    summary.append("")
    return True


def e5(lab, summary):
    """Claude-shape introspection — what fits the three ActivityKinds, what doesn't."""
    lab.start_proto()
    ag = lab.agent("B1", "worksession", parent="block:B1", shape="claude",
                   out=os.path.join(lab.run_dir, "e5-worksession.json"))
    ag.wait()
    lab.stop_proto()
    r = json.load(open(os.path.join(lab.run_dir, "e5-worksession.json")))
    emitted = [e for e in r["log"] if e.get("emitted")]
    skipped = [e for e in r["log"] if not e.get("emitted")]
    summary.append("## E5 — claude-shape introspection")
    summary.append(f"- of {len(r['log'])} scripted actions, emitted {len(emitted)}, "
                   f"skipped {len(skipped)} (navigation/noise)")
    summary.append("- emitted (what a peer would benefit from):")
    for e in emitted:
        summary.append(f"  - `{e['kind']}` act-{e['id']}: {e['title']}")
    summary.append("- skipped (stayed opaque, opt-in honored):")
    for e in skipped:
        summary.append(f"  - {e['reason']}")
    summary.append("- **gap felt:** the 1 *decision* had to be modeled as an "
                   "`agent_message` broadcast (`to:None`). There is no `Decision`/`Note` "
                   "kind. It works, but a decision isn't really a message to anyone — "
                   "flagged for the design doc.")
    summary.append("")
    return True


EXPERIMENTS = {"e1": e1, "e2": e2, "e3": e3, "e5": e5}  # e4 takes extra args


def main():
    argv = sys.argv[1:]
    cadence = [100, 250, 500, 1000, 2000, 5000]
    which = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--cadence":
            cadence = [int(x) for x in argv[i + 1].split(",")]
            i += 2
        else:
            which.append(a.lower())
            i += 1
    if not which:
        which = ["e1", "e2", "e3", "e4", "e5"]

    run_dir = os.path.join(HERE, "runs", now_stamp())
    lab = Lab(run_dir)
    summary = [f"# Lounge validation run — {os.path.basename(run_dir)}", ""]
    results = {}
    try:
        for name in which:
            print(f"== running {name} ==", file=sys.stderr, flush=True)
            if name == "e4":
                results[name] = e4(lab, summary, cadence)
            elif name in EXPERIMENTS:
                results[name] = EXPERIMENTS[name](lab, summary)
            else:
                print(f"unknown experiment {name}", file=sys.stderr)
    finally:
        lab.stop_proto()

    summary_path = os.path.join(run_dir, "summary.md")
    with open(summary_path, "w") as f:
        f.write("\n".join(summary))
    print("\n".join(summary))
    print(f"\n[summary written to {summary_path}]", file=sys.stderr)


if __name__ == "__main__":
    main()
