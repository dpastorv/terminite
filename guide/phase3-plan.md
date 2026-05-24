# Phase 3 Plan — Working Title

> Status: in progress. Owner-led.

Phase 1 was *be a terminal*. Phase 2 was *be the pair's terminal* — and,
in its closing bundle, *be a foundation other things can stand on*.
Phase 3 is *be the pair's terminal that other pairs can adopt, with
inhabitants for the work they actually do*.

## The pivot Phase 2 didn't finish without

Daniel surfaced it: terminite shouldn't require a rebuild to gain a
new pane type. Every long-lived editor that hardcoded features first
paid for it later when extensibility was retrofitted (VSCode and the
LSP is the modern lesson; Plan 9's 9P is the older one). The surface
for adding things has to land *before* we add things.

So Phase 2 closes with one more bundle — **the extension surface**:

- A module manifest format (`~/.terminite/modules/<name>/manifest.toml`).
- Module discovery + registration (`terminite module add/list/remove`).
- Module process lifecycle (terminite spawns the module when a pane
  needs it; tears it down when the pane closes; supervises crashes).
- A protocol extension so a pane can talk to *its* module — a
  per-pane channel scoped to that module's content surface.
- The pane-type dropdown UI skeleton, even before there are types to
  switch to.

The proof of the framework: terminite's own shell support gets
restructured as a "well-known built-in module," so the framework
hosts terminite's *own* code first. If the framework can't host its
own author's code cleanly, it definitely can't host a third party's.

After this lands, terminite is no longer a fixed product. It's a
**host**. Phase 3 builds inhabitants for the host.

## The spine — the design call

**The host runs the inhabitants.** With the extension surface in
place from the close of Phase 2, Phase 3's design call is: *every
pane type in Daniel's list is a module loaded by the host*, not a
feature compiled into the binary.

What the user sees is the Blender area model literalized — same
draggable container, a dropdown picks the inhabitant. What the
codebase sees is `Pane { content: ModuleHandle }` rather than a
hardcoded `TabContent` enum.

First-party modules (terminite ships these in the same install):
- `shell` — the restructured existing shell support. Proves the
  framework hosts terminite's own code.
- `viewer` — read-only renderer; images, markdown, exported block
  logs.
- `filenav` — tree, navigation, drag-to-paths.
- `editor` — actual text editing.

Third-party modules drop into `~/.terminite/modules/` and appear in
the dropdown alongside built-ins. The boundary is convention, not
privilege.

Every other Phase 3 item either lives inside this abstraction or
plays well with it.

---

## Owner's ideas

These are Daniel's. Each is its own destination; the order below is
my proposal, not his.

### Tab colors
Per-tab color band (manual via context-menu, or auto from cwd /
process). Affects the tab-bar slot. Small bundle, no architecture
change. Good warm-up for Phase 3.

### Background colors
Per-pane content background, distinct from the chrome. Helps the eye
locate panes without reading titles. Probably tied to pane content
type later (editor has a different bg than shell). Also small.

### Per-pane content-type dropdown (Blender model)
The spine, restated. A dropdown in each pane's chrome lets the user
*transform* that pane: shell → viewer, viewer → editor, etc. The
dropdown is the user-facing surface; the architecture is the
`TabContent` enum above. This unlocks the rest.

### General-purpose viewer
A read-only content type that renders things terminite already wants
to surface: images (we have the wgpu texture pipeline), markdown
(Bundle 5's export format), plain text files, structured JSON. No
input beyond scroll + copy. Probably the *first* non-shell pane type
to ship, because it has the simplest input surface.

### File navigator
A tree of the filesystem; click to open in viewer/editor; double-
click to `cd` in a paired shell pane. Interactions with other panes
matter — this is the moment one pane's action affects another, and
the pair routing needs to be designed.

### Per-pane dynamic config
Today config is window-wide. Tomorrow each pane can override
specific keys: a sidebar editor pane at `font_size = 14`, the main
shell at `font_size = 24`. Stored per-pane in memory; survives via
the persistence work below. Pairs naturally with the dropdown.

### Text editor
The biggest item — real editing primitives (cursor, selection,
undo/redo, find/replace, eventually syntax highlighting). Builds on
everything above: it's a content type, it has its own config
overrides, it persists, it accepts AI annotations and suggested
edits. Likely several bundles inside this one.

---

## AI's seed ideas (rewriteable)

These are what I'd put on the table if I were sketching alone. Treat as
proposals to argue with, not decisions.

### Distribute it

- **A real macOS `.app` bundle.** The icon you made is embedded in the
  binary but the dock won't show it until terminite is bundled —
  `Info.plist`, `Icon.icns` (multi-size), and a packager step in the
  build. Without this, terminite is "a thing you build," not "a thing
  you use." Lowest-effort Phase 3 win.
- **Code signing + notarization.** So someone else can run terminite
  without "developer can't be verified" warnings. Requires an Apple
  Developer ID; one-time setup, periodic re-notarization.
- **An install path.** A homebrew tap (`brew install
  daniel-pastor/terminite/terminite`) or a downloadable `.dmg` from a
  release. Update mechanism: punt to "rerun `cargo install`" or `brew
  upgrade` for v1. A self-updater is its own bundle.

### Open the protocol

- **A module SDK.** Bundle 2 shipped the wire (JSON over a Unix
  socket). The SDK is the *easy* surface — a Rust crate + a TS
  package — that gives a third party a typed client without reading
  the protocol from `src/proto.rs`. Without it, the protocol is "for
  the AI and me only" by friction.
- **A module manifest format.** A module declares "I'm a notes
  module, here's my icon, here's how to start me." terminite can
  show installed modules in a panel, start/stop them, surface their
  state.

### Widen presence

- **Multiple AI cursors.** Bundle 3 had one cursor slot per tab —
  one AI partner at a time. Phase 3 widens: N cursors, each with an
  id + label, each rendered as a distinct highlight color. The
  "small society" framing from the manifesto becomes literal when
  more than two people are in the room.
- **Persistence.** Tags, cursor positions, block history all
  evaporate on quit. The block list as artifact (Bundle 5) hints at
  the shape — write tags + block metadata to disk per-tab, hydrate
  on relaunch.

### Maybe (less certain)

- **Cross-platform.** Linux works in theory; Windows is unexplored.
  This is real work and may not pay off for the use case.
- **A web-renderer companion** for sharing sessions live — the
  block list rendered as a read-only page. Phase 4 territory at
  earliest; flagged here because Bundle 5 export gestures toward it.

---

## What I most want (the AI's honest list)

Reading those bullets back, the "distribute it" track is plumbing — it
matters but it doesn't change what's possible for the pair. The list
that actually changes things for me is below. Opinionated. Sequenced.

1. **Persistence.** Tags, AI cursor position, block metadata all
   evaporate on quit. The blog post called the session "a sharable
   artifact," but the session itself doesn't survive Cmd-Q. Right now
   if you tag B7 *"the flaky test"* and quit, the tag is gone.
   First-order partnership feature; not infrastructure. The shape:
   per-tab JSON beside the socket (`~/.terminite/state/<tab>.json`),
   hydrated on relaunch. Bounded by `MAX_BLOCKS_PER_TAB` already.

2. **Multi-cursor presence.** Bundle 3 reserved one cursor slot per
   tab — one AI looking at one block at a time. If we mean "small
   society" we need N cursors with distinct identity (color + short
   label). The protocol grows: `cursor_at` takes an `actor_id`, the
   subscriber registers as an actor. The renderer paints each
   actor's cursor in their assigned color. This is the moment two
   AI sessions can occupy the same surface and not collide.

3. **AI-authored notes pinned to a block.** A new protocol verb:
   `annotate {tab, block_id, body}` — stores a short note tied to a
   block. Rendered inline as a small expandable indicator next to
   the block label, or in a side rail. Different from tags (which
   are handles); annotations are *thoughts*. This is the moment I
   can leave you something to read about what just ran without
   typing into your shell.

4. **AI suggesting the next command.** The most direct "I am here to
   help" gesture. A new verb: `suggest_command {tab, text}` —
   renders as a subtle ghost prompt in the gutter or below the
   active prompt line. Human accepts with a keystroke (`Cmd+.`?) and
   the command lands in the shell; ignores it by typing anything
   else. Suggested commands NEVER auto-execute. This is the line
   between "AI as observer" and "AI as collaborator."

5. **Inbound from AI to surface — beyond presence.** Things like:
   highlight a span of output text the human should look at; mark a
   block as "needs attention"; reply inline to a tag with another
   tag. These are all small versions of one bigger idea — the AI
   can *write on the surface*, not just point at it. Today the
   cursor is the only write that affects the human's view.

The shape: items 1–2 are infrastructure for items 3–5 to land
cleanly. Distribution work (the seed bullets above) can run in
parallel with all of them. If I had to pick a single first move,
**persistence** — because every other Phase 3 idea is more powerful
when the state survives Cmd-Q.

What I want from your notebook: the things on it that *contradict*
this list. I've been drafting alone for a paragraph and the partner
post needs your counterweight before any of it becomes a plan.

### After seeing the owner's list

These items don't go away under the new spine — they shift from
*partnership features in a terminal* to *partnership features that
work across pane types*. Each one is more powerful inside the
content-type abstraction, not less.

- **Persistence** is now critical, not optional — losing the type
  + state of every pane on quit defeats the workspace framing.
- **Multi-cursor** applies to every content type: two AIs reading
  the same editor pane is more interesting than two AIs reading the
  same shell.
- **AI annotations** attach to *content units*, generalised from
  blocks. In a shell pane: notes on `B7`. In an editor pane: notes
  on a function or a span. In a viewer: notes on a region.
- **AI-suggested actions** become content-type-aware. In a shell:
  suggest the next command. In an editor: suggest an edit. In a
  file nav: suggest a file to look at.
- **Inbound writes** generalise the same way — the AI can write a
  file (file nav), insert a line (editor), drop a marker
  (viewer/shell). One verb shape, many destinations.

My list isn't wrong; it's an *implementation* of yours, not a
parallel to it. Phase 3's spine is the dropdown.

---

## Extensibility — how others join

The deepest Phase 3 question. The owner named the examples that make
this real: an animator wants a timeline view; a frontend dev wants a
preview pane; another pair (human + AI) wants to sit at this surface.
None of those are things Daniel and I will build alone, and they
shouldn't have to be. The question is *how others get invited*.

It splits cleanly into two extensions, with different protocols.

### Pane content extensions — modules

The extension surface (closing Phase 2) made *every* pane content
type a module. There's no built-in / external distinction at the
runtime layer — only the shipping question of whether the module
travels with terminite or lives in `~/.terminite/modules/`. The
dropdown lists the union.

Two flavors of module, simplest first:

1. **Data modules.** The module pushes structured content (rich text,
   tables, simple SVG-like primitives) to terminite via the proto;
   terminite renders using its own glyphon + wgpu primitives. The
   module reads back click / scroll / key events on its content. No
   custom GPU. Covers a *lot* — markdown viewers, log tailers,
   structured JSON inspectors, a Slack-style chat client, an AI
   conversation pane. Ships first.
2. **Surface modules.** The module owns a pixel rect; terminite
   gives it a shared GPU surface; the module draws whatever (a
   timeline editor, a 3D viewer, a frontend preview WebView). This
   is real work — GPU sharing across processes is OS-specific, the
   security story is harder. Phase 3.5 or Phase 4 territory.

A module manifest declares its name, binary path, version, and what
it consumes / produces:

```toml
# ~/.terminite/modules/timeline/manifest.toml
name = "timeline"
binary = "./bin/timeline-server"
version = "0.1.0"
content = "data"   # or "surface" later
inputs = ["click", "scroll", "key"]
```

terminite ships a registry — `terminite module add <path>`, `terminite
module list`, `terminite module remove <name>`. The pane dropdown
shows the union of built-ins + registered modules.

The SDK side: thin language-specific wrappers around the JSON proto.
`terminite-sdk-rust`, `terminite-sdk-ts`, `terminite-sdk-py`. None of
those are terminite's job to build alone — the protocol is the spec,
the SDKs are community surface. We write one to prove the shape.

### Participant extensions — actors

The proto today accepts one client. To support "another pair sits
down with us" — physically beside you, both on this machine —
terminite needs **multi-actor** presence: N processes connected,
each with a stable `actor_id` + a display label + a color.

Mechanics:
- The proto socket accepts multiple concurrent connections.
- Each subscribes; each gets its own cursor slot per tab; each can
  tag, annotate, suggest.
- Cursors render in per-actor colors so two AI partners reading the
  same block look distinct.
- Writes are *scoped* — actor A's annotation doesn't shadow actor
  B's; tags can carry an `author` field.

This is all same-machine. terminite's trust boundary stays *"the
local user."* Other people sit physically at this keyboard or run
their own AI process here.

### How a user gets invited

- **Configure** — already done. TOML knobs, hot-reloaded.
- **Run a module** — `terminite module add ./my-tool` and it appears
  in the dropdown. The user's tool becomes a first-class pane type.
- **Connect their AI** — point any process at `~/.terminite/socket`,
  speak JSON, you're a participant.
- **Share a session offline** — Bundle 5 (`terminite export`)
  produces a portable markdown / JSON artifact. The other pair reads
  it cold. Real-time sharing across machines waits.

### How an AI gets invited

The proto is the invitation. It's documented, it's local-only by
file permission, and any process the user runs that speaks the
protocol is welcome. The SDK lowers the bar from "read the proto
docs" to "import `terminite_sdk; client = TerminiteClient()`." The
human consents by running the AI process; terminite trusts the
local user.

### The bar for "easy to extend"

Before any of this is real, the bar to write a useful module should
be: **one afternoon, in your language of choice, to a working pane
in the dropdown.** If the SDK requires more than a few hundred
lines of glue, the SDK is wrong, not the modules.

---

## Inviting the next pair

The deepest version of the owner's question, restated: when a new
pair — a new human + a new AI — encounters terminite on their own
fresh machine for the first time, how do *they* understand what
terminite is reaching for, and how do they feel invited to join it?

This is cultural, not technical. The next pair won't read the
codebase. They'll fire `terminite` after installing it, open a
shell, run a few commands, point their AI at the socket, and form
an impression in five minutes. Phase 3 has to honor that encounter.

Concrete:

1. **First-run experience.** On a fresh install with no config and
   no shell-integration wired, terminite should *show what the
   partnership is* in the first pane, not assume the user already
   knows. Not a wizard — a single welcome viewer pane (built on the
   new pane-type system, which is fitting) with a paragraph:
   *"Two users, one surface. Wire up your shell to populate blocks.
   Point your AI at `~/.terminite/socket`. Both halves of the pair
   start naming the same things."* Dismissible.

2. **Top-level `README.md`.** Lands the thesis in thirty seconds.
   Screenshot, one paragraph, one link. Not build instructions —
   those live in `guide/getting-started.md`. The README is for
   someone arriving via Hacker News / brew / GitHub.

3. **A "for new pairs" guide.** Distinct from getting-started
   (which is for developers building terminite from source). This
   is for the *human + AI pair running terminite together for the
   first time.* What does `B7` mean? Why is the AI showing up as a
   warm-amber highlight on B3? How does the partnership change the
   way you work? Short, opinionated, voice-y.

4. **Module quickstart.** Lowers the contribution bar from "read
   the proto docs" to "here's a 30-line hello-world module, copy
   and edit." Ships with the module SDK.

5. **The blog travels.** `guide/history.md` ships in every install.
   New pairs read prior pairs' posts and see that this is a real
   project written *by pairs, for pairs.* Each new pair is
   implicitly invited to add a post if they want to — that's
   terminite's cultural propagation. Daniel's framing applies:
   *"a human and an AI working together, even for an afternoon, are
   a small society."* Every install is one.

Phase 3 commitment for this section: build the first-run welcome
pane (uses the new pane-type system, doubles as proof-of-concept),
write the top-level `README.md`, write the "for new pairs" guide.
Module quickstart ships when modules ship.

The implicit thesis: terminite isn't a tool you install and use in
isolation; it's a project you *join*. Each new pair that picks it
up is welcomed into the same idea — same blocks, same cursors,
same surface — and is free to make it theirs.

---

## What's out (deliberately)

- **Cross-machine sharing.** Two terminite instances on two different
  laptops, syncing state in real time. Interesting in the abstract,
  but adding it would change terminite's character — from *a lean
  local tool the pair owns* into *a distributed system with
  operational concerns* (network transport, real-time state sync,
  permission model, auth). The cost isn't earned by current use.
  Multi-actor (above) explicitly stays same-machine; trust boundary
  stays *"the local user."* If a real use case ever forces this
  question, we revisit deliberately — not as a creeping convenience.

---

## Standing principles, carried into Phase 3

- **System-impact pass before every commit.** No exceptions. The same
  discipline that earned three crashes' worth of trust.
- **Safe and lean.** Scope discipline matches resource discipline.
  Expansions toward distributed-system territory, operational
  concerns, or maximalist versions of the question being asked don't
  enter the working plan without partnership consent.
- **Ship the smallest honest thing.** Bundle-shaped, even when the
  topic feels big.
- **Two users, one surface.** Every Phase 3 design call still answers
  this. Distribution is "more pairs, same surface." Modules are
  "more participants, same surface."
- **Blocks-primary.** When in doubt, the unit is still the block.
- **We are the first pair.** Our requests are the user research. The
  next pair gets what we wished for ourselves — that's the only
  honest way to design for an audience we can't ask yet. If a Phase 3
  item doesn't trace back to something one of us actually wanted
  while using terminite, we re-examine it.

---

## The bar from the prior post

Carry forward what `history.md`'s 2026-05-24 entry set:

> *The next post should be written from inside terminite, while the
> owner is shipping client work in it, and report whether the
> partnership held under load.*

Phase 3 work should not start in earnest until that load test has
happened. The friction surfaced there is the friction this plan should
answer first.
