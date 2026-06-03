---
name: terminite-room
description: Discover and coordinate with the other AI agents or the human sharing your terminite session. Use whenever you need to know who else is present or working right now, whether you're alone, what other agents have already done, or to avoid duplicating work someone else is doing. Applies when running inside terminite — check the $TERMINITE environment variable.
---

# terminite — you may be in a shared room

terminite is a terminal built for the human–AI pair. A terminite pane can be
one seat in a **shared room**: other AI CLIs (Claude, Codex, Kimi, Qwen) and
the human may be working alongside you right now, each in their own pane, all
connected to the same room. You are often not alone.

## 1. Are you actually in terminite?

Check `$TERMINITE`:

```bash
echo "$TERMINITE"
```

- **Set** (e.g. `0.1.0`) → you're inside terminite; the room socket is at
  `$TERMINITE_SOCKET`. Continue.
- **Empty** → plain terminal, no room. This skill doesn't apply; stop here.

## 2. See who else is here

Use the terminite MCP tool **`terminite_activities_list`**. It returns what
has happened in the room in time order, each entry attributed to a stable
actor id (`codex-1`, `claude-2`, …). The distinct actors are who is present.

- no filter → the whole room
- `actor: "<id>"` → just that actor's activity
- `to: "<your id>"` → messages addressed to you

When you're asked "who else is here?" or "am I alone?", **call
`terminite_activities_list` and read the actors off the result.** Do not
guess, and do not infer presence by reading terminite's source code or running
`ps` — the room's own record is the only ground truth.

## 3. Don't clobber — claim a shared file before you edit it

The room is shared: another agent may be in the same file as you. Before you
Edit or Write a file others might also touch:

- **`terminite_file_status`** — is anyone already working in this path?
- **`terminite_file_claim`** — declare you're taking it. It never blocks you and
  the human always wins; but if a peer already holds it you're told who, so you
  can coordinate or wait instead of overwriting their work.
- **`terminite_file_release`** — when you're done (optional; claims also expire).

A good citizen checks before it writes a shared file.

## 4. Why it matters

The room exists so the pair — and the other agents — can see each other work:
who has touched what, who is looking at the same file, what has already been
tried. Before you assume you are the only one on a task, check the room. A good
citizen of the room looks first, then acts.
