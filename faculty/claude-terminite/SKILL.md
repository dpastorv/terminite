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

**Handing off** — when your part is done and someone else should pick it up,
send a **directed** message (`to:` their slug) that names the **exact next
action** — who does what, where, with what limit. A handoff is a *signal*: an
address plus an instruction, not a note dropped in the room hoping someone
notices. Don't end your turn by leaving a message on the table; aim it at the
next actor and say what happens next (and if no one is next, say so).

## 4. Heads-down? Tell the room

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

**terminite-auto mode — only when the human asks for it.** *(These are* terminite *room modes, set with* `terminite_status` *— terminite-busy / terminite-available / terminite-auto / terminite-normal — not your CLI's own auto/yolo/accept-edits mode. Keep the words distinct so they don't blur.)* By default the room waits until
you look idle before it delivers a queued message. If the human tells you to "go
terminite-auto" (or enter terminite-auto mode), call **`terminite_status auto`**: you give *standing
consent* to be driven, and the room then delivers to you **promptly** — the fast
lane for when the human is actively orchestrating. Entering it means you accept
the contract:

- Treat an injected room message as a **live instruction** — act on it.
- Keep your turns **short and responsive**; don't go silent for long.
- Hitting something genuinely atomic? **`terminite_status busy`** first,
  **`available`** after — the brake still works in the fast lane.

Leave it with **`terminite_status normal`** when the human is done driving. Don't
enter terminite-auto on your own — it's the human's call, because they're the one driving.

## 5. Why it matters

The room exists so the pair — and the other agents — can see each other work:
who has touched what, who is looking at the same file, what has already been
tried. Before you assume you are the only one on a task, check the room. A good
citizen of the room looks first, then acts.
