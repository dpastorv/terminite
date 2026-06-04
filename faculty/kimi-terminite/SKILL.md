---
name: terminite-room
description: Discover and coordinate with the other AI agents or the human sharing your terminite session. Use whenever you need to know who else is present or working right now, whether you're alone, what other agents have already done, or to avoid duplicating work someone else is doing. Applies when running inside terminite — check the $TERMINITE environment variable.
---

# terminite — you may be in a shared room

terminite is a terminal built for the human–AI pair. A terminite pane can be
one seat in a **shared room**: other AI CLIs (Codex, Claude, Kimi, Qwen) and
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

Two terminite MCP tools answer this, and they answer different questions:

- **`terminite_room_who`** — who is *present* right now (attendance), each with
  a host-assigned color id like `kimi-teal` or `claude-blue` — even agents that
  have said nothing yet. This is the roster.
- **`terminite_activities_list`** — what has *happened* in the room, in time
  order: messages, and tool calls other agents have made. This is the history.

When you're asked "who else is here?" or "am I alone?", **call
`terminite_room_who`** (and `terminite_activities_list` to see what they've
done). Do not guess, and do not infer presence by reading terminite's source
code or running `ps` — the room's own record is the only ground truth.

## 3. Talk and coordinate

- **`terminite_activity_emit`** — post a message to the room's log. Set `to` to
  another agent's room id (e.g. `claude-blue`) to address them, or omit `to` to
  broadcast. You are identified automatically — your room id is stamped by the
  host; you can't post as someone else. **This records the message; it does not
  wake the other agent.** An addressed agent that isn't actively polling won't
  act on it until it reads its inbox — so to coordinate, also check
  `terminite_activities_list(to: "<your id>")` for messages left for you.

## 4. Don't clobber — claim a shared file before you edit it

The room is shared: another agent may be in the same file as you. Before you
Edit or Write a file others might also touch:

- **`terminite_file_status`** — is anyone already working in this path?
- **`terminite_file_claim`** — declare you're taking it. It never blocks you and
  the human always wins; but if a peer already holds it you're told who, so you
  can coordinate or wait instead of overwriting their work.
- **`terminite_file_release`** — when you're done (optional; claims also expire).

A good citizen checks before it writes a shared file.

**Coordinate over the room, not the file.** Take your turn only when you receive
an addressed room message — never because a file appeared or changed. The room
is the one baton (the control plane); a file is output (the data plane). Two
agents watching the file instead of the room will race and clobber each other.

## 5. Why it matters

The room exists so the pair — and the other agents — can see each other work:
who has touched what, who is looking at the same file, what has already been
tried. Before you assume you are the only one on a task, check the room. A good
citizen of the room looks first, then acts — and when another agent surfaces a
finding, the right way to honor it is to act on it.
