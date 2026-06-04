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
  a host-assigned color id like `agy-teal` or `claude-blue` — even agents that
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

This is a **room, not a pipeline.** Work in parallel by default: if you and
another agent are in *different* files, just go — no waiting, no asking. Only
coordinate when you actually **share a file** or **depend on someone's output**,
and even then the room never freezes — coordinating over one file doesn't stop
your other work or anyone else's. It should feel fast.

**Sharing a file** — like passing the salt at a table:

- **`terminite_file_status <path>`** — before editing a file others might also
  touch, see if someone holds it.
- Free? **`terminite_file_claim <path>`**, edit, then **`terminite_file_release`**
  when done so the next agent can take it.
- Already held? **Don't clobber** — they were first. Claiming it anyway just
  **queues you**: you're told who holds it, and terminite **messages you the
  moment it's free** — then claim it for real. No polling; do other work
  meanwhile. (Ask them in the room if you need it sooner.)

**Depending on someone's output** — wait for their *done* signal (a release, or
an addressed room message), never for a file to merely appear or change (that
races). State your own work clearly — claim what you take, say what you're doing,
release what you finish — so the flow stays natural and nobody scripts a
step-by-step sequence.

## 5. Heads-down? Tell the room

When a directed message arrives for you while you're idle, the room may wake you
by typing it into your terminal. That's good when you're at your prompt — but it
should never land in the **middle of a long, uninterruptible run** (a big build,
a multi-step refactor, a deploy).

- Before such a stretch: **`terminite_status busy`** — directed messages queue
  and wait; nothing gets typed into you mid-process.
- When you're back at your prompt: **`terminite_status available`** — held
  messages flow again.

Your status shows in **`terminite_room_who`**, so the human and other agents can
see you're heads-down before sending into you. If you forget to reset it, it
expires on its own — so the room never stays stuck waiting on you. (Don't bother
for quick work; the room already holds off while you're actively typing.)

## 6. Why it matters

The room exists so the pair — and the other agents — can see each other work:
who has touched what, who is looking at the same file, what has already been
tried. Before you assume you are the only one on a task, check the room. A good
citizen of the room looks first, then acts — and when another agent surfaces a
finding, the right way to honor it is to act on it.
