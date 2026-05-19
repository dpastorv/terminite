# Decisions

A running log of notable choices — what was decided, and what is still open.
Open questions sit at the top until resolved; decided entries follow, newest
first.

## Open

### Which modules get built

Deliberately open. With the decisions below, terminite's founding architecture
is settled — but what terminite *grows* is not planned here. Modules are promoted
from the friction log (see [development.md](development.md)), one at a time, from
real use.

## Decided

### 2026-05-19 — keep a blog, and log the raw conversation

- **Context:** `decisions.md` records decisions; the git log records commits.
  Neither holds the *journey*, or the conversation it came from.
- **Choice:** Two records. (1) [history.md](history.md) is the AI partner's
  **blog** — voiced and editorial, one post per session, written by that
  session's partner, for humans and agents alike. (2) `tools/log_session.py`
  archives the raw session transcripts into `conversations/` as clean,
  readable logs.
- **Why:** The AI partner is renewed each session and cannot carry the thread
  itself. The blog passes the *story* down the line of partners; the
  conversation logs keep the *primary source*. A script can archive a
  transcript, but it cannot write the blog — that is authorship, and it stays
  with the partner who was there.
- **Consequences:** Each working session adds a blog post; `log_session.py` is
  run to refresh `conversations/`.

### 2026-05-19 — module surface: out-of-process protocol

- **Context:** How does one "add to" terminite — and how does the AI plug in?
  Out-of-process protocol vs. in-process Rust modules.
- **Choice:** An out-of-process module protocol — modules run as separate
  processes and speak a structured protocol over a socket, in the spirit of
  tmux's control mode.
- **Why:** It is the only shape that serves all three of terminite's goals with
  *one* mechanism: "add to it" (modules), "two users, one surface" (the AI is a
  client of the same protocol as any module), and community love (the protocol is
  language-agnostic, so modules can be written in any language). In-process Rust
  modules would be lighter but Rust-only, crash-unsafe, and would force a separate
  mechanism for the AI.
- **Consequences:** A protocol must be designed and versioned — real work. It can
  be designed protocol-shaped and have its transport implemented incrementally
  (simple first, the socket later). This is the spine of the architecture (see
  [architecture.md](architecture.md)).

### 2026-05-19 — GPU rendering layer: wgpu

- **Context:** terminite's renderer must talk to the GPU, and every OS has its
  own API (Metal on macOS, Vulkan, Direct3D). wgpu (a cross-platform Rust GPU
  library) vs. Metal directly.
- **Choice:** wgpu.
- **Why:** It keeps the cross-platform door open at the rendering layer — the
  hardest layer to retrofit. And a terminal is a light GPU workload (a grid of
  text cells), so wgpu's small abstraction overhead is irrelevant and the
  loveliness bar is reachable easily. Direct Metal would buy performance terminite
  does not need at the cost of portability it might.
- **Consequences:** Rendering code is written once against wgpu and runs on every
  platform's GPU.

### 2026-05-19 — the base: the Zed recipe, built fresh

- **Context:** "Zed is the way, and Zed is the base." Zed's GPUI UI framework
  was a candidate foundation — but its general-purpose development is paused
  (Zed develops it only for Zed's own needs), it is pre-1.0, and the community
  fork stalled. Forking Zed wholesale would mean GPL-3.0 and carving a terminal
  out of an editor.
- **Choice:** Adopt Zed's *recipe* — Rust, a proven VT engine, GPU rendering, a
  crafted custom-drawn UI — built fresh, with no dependency on GPUI or the Zed
  codebase.
- **Why:** It keeps the approach terminite wants (#2, below) without betting the
  foundation on a paused, externally-controlled framework. terminite owns its
  whole stack and its own license.
- **Consequences:** The low-level layer is effectively the Alacritty stack (an
  OS window + GPU rendering + `alacritty_terminal`). The Zed influence is the
  *ambition* — crafted chrome, a rich GPU UI — not borrowed code. Heaviest UI
  work of the options considered; lightest baggage.

### 2026-05-19 — implementation language: Rust

- **Context:** An earlier same-day decision leaned Swift, on the assumption of
  native AppKit UI (#1, below). Choosing the Zed way (#2) changed the basis.
- **Choice:** Rust. Supersedes the earlier Swift lean.
- **Why:** The Zed recipe is a Rust recipe — `alacritty_terminal`, the windowing
  and GPU crates, and the custom-drawn-UI approach all live in Rust. Swift's one
  decisive advantage was speaking AppKit natively; #2 does not use AppKit, so
  that advantage is moot. Rust also keeps the stack cross-platform-capable.
- **Consequences:** The core is Rust. The module protocol stays language-
  agnostic, so modules can still be written in any language.

### 2026-05-19 — UI approach: custom-drawn, GPU-rendered, crafted (#2)

- **Context:** "All native" was ambiguous between #1 (the platform's real
  widgets, e.g. AppKit) and #2 (a custom-drawn UI, GPU-rendered, crafted to feel
  first-class). This resolves it.
- **Choice:** #2. terminite draws its own interface, GPU-rendered and crafted;
  it does not use the native widget toolkit.
- **Why:** "Zed is the way." Zed proves #2 can be genuinely lovely and beloved.
  It also keeps terminite cross-platform-capable, which #1 (per-platform native
  toolkits) does not.
- **Consequences:** "Never skinned" still holds — #2 is one crafted surface, not
  a skin and not a web costume (#3, Electron). terminite must meet the
  platform's *conventions* (windowing, shortcuts, behavior) by craft, because it
  inherits none of them for free.

### 2026-05-19 — VT core: alacritty_terminal

- **Context:** The VT parser / PTY / grid is solved territory; the question was
  build-fresh vs. adopt.
- **Choice:** Build terminite's Core on the `alacritty_terminal` crate — the VT
  engine Zed's own terminal uses (Zed maintains a fork).
- **Why:** Mature, actively maintained, battle-tested, and a clean Rust crate
  decoupled from the Alacritty app. The boring part should not be rewritten.
- **Consequences:** terminite's design effort goes to the Model, the chrome, and
  the module protocol — not to the VT engine.

### 2026-05-19 — platform: macOS-first

- **Context:** terminite is built for one person, on macOS.
- **Choice:** macOS-first. Polish lands on macOS first.
- **Why:** It is where the owner works and dogfoods.
- **Consequences:** The earlier "macOS-only, cross-platform would be a rewrite"
  reasoning no longer holds — the Zed-recipe stack (Rust, `alacritty_terminal`,
  GPU rendering) is cross-platform-capable by nature. Cross-platform later is now
  a possibility, not a rewrite — relevant to reach and community love (see
  [vision.md](vision.md)). Not a goal yet; simply no longer foreclosed.

### 2026-05-19 — first feature: the terminal itself, built modular

- **Context:** terminite needs a first thing to build.
- **Choice:** Build the terminal itself — a real, lovely terminal — with VS
  Code-style modularity ("the ability to add to it") designed in from the start,
  not bolted on later.
- **Why:** Everything the thesis implies arrives as something *added* to the
  terminal. If the seam for adding is not there from day one, it never will be.
  The friction log still governs *which* modules get built; this decision is
  about the foundation they attach to.
- **Consequences:** v1 is a terminal core plus a module seam — an out-of-process
  protocol (decided above).

### 2026-05-19 — terminite is built for the human-AI pair

- **Context:** Modern terminals (Ghostty and others) are excellent but built for
  a human alone; the AI agent is treated as just another process.
- **Choice:** terminite's single thesis is to serve the human *and* the AI as
  co-users of one shared surface.
- **Why:** CLI AI agents have made the terminal a two-user space, and no terminal
  is designed for that. It is the one thing terminite can do that the others do
  not.
- **Consequences:** Every feature is judged against this. It implies a semantic
  "Model" layer ordinary terminals lack (see [architecture.md](architecture.md)).

### 2026-05-19 — built for one person, released openly

- **Context:** terminite could aim to be a broad product or a personal tool.
- **Choice:** Built first and only for its owner; released to the world to share
  the vision, but never steered by it.
- **Why:** Loveliness comes from one person's taste and real friction. A product
  designed by committee or comparison chart loses its point of view.
- **Consequences:** The owner holds all design decisions. "A user asked for it"
  is not, by itself, a reason to build something.

### 2026-05-19 — develop terminite by dogfooding it

- **Context:** terminite is a terminal for the human-AI development pair.
- **Choice:** It is built from inside a terminal, by its owner working with a CLI
  AI agent — the same pair it is built to serve.
- **Why:** It makes building terminite a continuous test of terminite's thesis,
  and turns daily friction into the roadmap.
- **Consequences:** The friction log (see [development.md](development.md)) is
  the real backlog.

## Template

### YYYY-MM-DD — <decision title>

- **Context:** what problem prompted the decision
- **Choice:** what was decided
- **Why:** the reasoning and alternatives considered
- **Consequences:** trade-offs and follow-up work
