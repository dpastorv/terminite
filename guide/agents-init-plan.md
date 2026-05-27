# Agents-init plan — making the partnership convention discoverable

> **Status: SUPERSEDED 2026-05-27.** This plan is preserved as the
> alternative we considered and decided against during the same-day
> alignment conversation. The conclusion: writing primer files into
> user projects (`AGENTS.md` / `CLAUDE.md` / `.cursorrules`) is the
> wrong layer — it asks the user to maintain per-project per-tool
> files and asks the AI to remember to *read* something. The right
> layer is the protocol itself (MCP server first, ACP client later)
> so the vocabulary self-advertises via tool descriptions in the AI's
> palette. See `guide/lounge-thesis.md` for what we landed on. This
> file stays as a record of the path not taken.


## The gap

terminite ships a shared-coordinate system between the human and the
AI — blocks, OSC 133 marks, cursor presence, tags, the proto socket.
The infrastructure works. The convention for *how each half uses it*
does not propagate.

A fresh AI session, opening on a fresh project where the user has
just installed terminite, will:

- Read whatever `CLAUDE.md` / `AGENTS.md` / `.cursorrules` it finds.
- Read source files when asked.
- Use Bash / Read / Edit tools as normal.

It will NOT:

- Discover `terminite tabs` / `blocks` / `cursor` / `tag` on its own.
- Realize that blocks are the shared coordinate.
- Place its cursor when focused on a block.
- Reference blocks by ID in conversation.

We verified this live — even with full memory files for this
project, Claude (me) didn't use blocks until Daniel said "how are you
going to use those." Without instruction, the partnership is
invisible.

## The proposal

Ship two things together:

1. **A canonical primer** (`guide/partner-primer.md`) — a short
   runbook the AI half of the pair reads on session start. Single
   source of truth, lives in terminite's repo.

2. **`terminite agents-init` verb** — writes a copy of that primer
   to the user's project root (`AGENTS.md` by default, or whatever
   filename the user's AI tool reads). Idempotent. Marker-guarded
   like `shell-init --install`.

The end state: a user clones their project, runs
`terminite agents-init --install`, fires up an AI agent, and the
agent reads `AGENTS.md` on session start and knows it's in a
terminite session + knows the partnership commands. The convention
travels with the project, not with the AI's memory.

## The primer's actual content (v1 draft)

```markdown
# Working in a terminite-instrumented session

You're in a project where the terminal is **terminite** — a terminal
built for the human-AI pair, with a shared coordinate system both
halves use. This file tells you how to participate.

## Sit-down ritual

Before doing anything substantial, get your bearings:

    terminite tabs                # which tabs exist
    terminite blocks <tab_id>     # recent activity, exit codes, tags

A "block" is one numbered shell command + its output (`B1`, `B2`…).
You and your human partner both refer to blocks by ID — when you say
"B7," they see the same gutter label. Shared coordinates, no
ambiguity.

## During work

- **Point** at blocks by ID in conversation ("B7's error…"). The
  human sees the gutter label and can click around it.
- **Place your cursor** on the block you're focused on:
  `terminite cursor <tab> <id>`. Renders as a warm-amber highlight
  in their gutter. Move it when you shift focus.
- **Tag** blocks worth marking:
  `terminite tag <tab> <id> <short-label>`. Persists in their
  layout file.
- **Subscribe** if you want events in real-time:
  `terminite watch` (streams `block_opened` / `block_closed`).

## When the workflow is "AI in a single shell"

If the human is running you interactively in *one* shell, that shell
never returns to a prompt, so OSC 133 only fires once — there's
effectively one open block (probably `B1`) that won't close until
you exit. Don't fight this. Use `B1` as the "this session"
coordinate; your tool calls render inline as text inside it.

For granular blocks alongside an AI session, the human should keep
a *separate* shell pane for their own commands.

## More

- Proto verbs: `terminite help`
- Architecture (longer read): `guide/architecture.md`
- Project history: `guide/history.md`
```

That's ~50 lines, small enough to fit comfortably in any AI session's
start-of-conversation context.

## Verb interface

```sh
terminite agents-init                       # print to stdout
terminite agents-init --install             # write AGENTS.md to cwd
terminite agents-init --install --path ./CLAUDE.md
                                            # write to a specific file
```

Idempotency: when `--install` writes, the content goes between HTML-
comment markers:

```html
<!-- >>> terminite partnership primer >>> -->
... primer body ...
<!-- <<< terminite partnership primer <<< -->
```

Re-running `--install` replaces only what's between the markers.
Pre-existing content in the file outside the markers is untouched —
same shape as `shell-init`. Users with their own AGENTS.md keep their
content; terminite's section slots in as a separate block.

## Honest limits

- **The AI has to read project-root files on session start.** Claude
  Code does; Cursor does; most agents do. Not universal. Tools that
  don't read project files won't get the primer.
- **One primer template.** v1 doesn't vary by AI tool — same content
  whether you're targeting Claude, Cursor, Aider. If specific tools
  need specific framings later, we add per-tool templates.
- **The primer is a starting point.** It's *editable* by the user
  after install. terminite's role is to provide a reasonable default,
  not to dictate.
- **No CLAUDE.md sniffing.** We don't auto-detect "this project
  already has a CLAUDE.md, append to it." Too magic. User runs
  `--path ./CLAUDE.md` if that's their target.

## Deliberately out of scope

- A human-side onboarding flow (`terminite new-project`, scaffolding
  a project from scratch). Separate concern. Users already have their
  own project structures.
- Per-AI-tool primer variants. One primer, vendor-neutral.
- Auto-update of the primer when terminite ships changes. The user
  re-runs `agents-init --install` when they want fresh content.
- Telemetry on whether the primer's commands actually get run.
  Out of scope by design — we wouldn't add it.

## Open questions for alignment

1. **AGENTS.md vs CLAUDE.md as the default filename.** AGENTS.md is
   the emerging vendor-neutral convention. CLAUDE.md is specifically
   what Claude Code reads. My lean answer: default to AGENTS.md,
   document the `--path` flag for users who specifically want
   CLAUDE.md or `.cursorrules`.

2. **Should the primer also point at terminite's `history.md`?**
   That's the partner-blog: each session's AI leaves a post for the
   next. It might be the natural "if you want to understand the
   project's culture, read this" pointer. My lean answer: yes, brief
   mention. Reading it isn't required, but pointing at it is honest.

3. **Should `agents-init` only run from a project root** (detect a
   `.git` directory) or from anywhere? Anywhere is more flexible;
   project-root detection is more guard-railed. My lean answer:
   anywhere — let the user decide where the file lands.

4. **Does the verb need a `--check` mode** to verify whether a
   project already has the primer installed and whether it's up to
   date? Probably yes if we ever update the primer template — but
   v1 can skip and add later when needed.

## What ships, lean

1. `guide/partner-primer.md` — the canonical primer, single source
   of truth.
2. `terminite agents-init` verb in `proto_client.rs`, mirroring the
   `shell-init` shape. Reads the bundled primer (via
   `include_str!`); prints or writes idempotently.
3. Mention in `guide/getting-started.md` under a new
   "AI partner onboarding" section, right after the shell-integration
   section.

Open question 1 above is the only real decision before building.
Everything else is honest defaults.
