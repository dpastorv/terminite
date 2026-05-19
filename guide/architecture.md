# Architecture

> **Status:** intent. No code exists yet. This describes how terminite is *meant*
> to be built; it hardens as the first lines are written. Decided pieces (Rust,
> the Zed recipe, custom-drawn GPU UI) and open ones live in
> [decisions.md](decisions.md).

## The stack

Decided: terminite is written in **Rust**, built on the **Zed recipe** — a proven
VT engine, GPU rendering, and a crafted custom-drawn UI — assembled fresh, with
no dependency on GPUI or the Zed codebase.

Built fresh, that recipe's low-level layer is effectively the **Alacritty stack**:

- **`alacritty_terminal`** — the VT engine (PTY, parsing, the cell grid).
  Adopted, not written. The VT core Zed's own terminal also uses.
- **winit** (or equivalent) — the OS window and input events.
- **wgpu** — GPU rendering. Metal under the hood on macOS, cross-platform by
  nature.

The Zed influence is the *ambition* — crafted chrome, a rich GPU-rendered UI,
editor-grade polish — not borrowed code. terminite is the Alacritty stack carried
to the Zed level of craft, plus the things that make it terminite.

## What the architecture has to make true

The architecture is downstream of the [vision](vision.md). It has to make the
thesis physically possible:

- **Render at the loveliness bar.** GPU-accelerated text, single-digit-ms input
  latency, smooth under heavy output. Solved territory — stand on the known-good
  approach.
- **Understand output as structure, not bytes.** A plain terminal keeps a grid of
  cells. terminite also needs a *model* of what is on screen — where a command
  began, where its output ended, where an agent's turn starts and stops.
- **Expose that structure to both users, and to modules.** The human sees it as
  rendering. The AI, and any module, must be able to read and drive it through a
  protocol. One mechanism, many clients.

## The spine (provisional)

```
  Shell             OS window (winit) + custom-drawn, crafted chrome
        │
  Renderer (wgpu)   draws the grid + the Model on the GPU
        │
  Multiplexer       sessions · windows · panes · persistent
        │
  per pane ──▶  Model   semantic layer — blocks, commands, agent turns
                Core    alacritty_terminal — PTY · VT parser · cell grid
        │
  Module protocol  ──  the Model, exposed; modules + the AI are clients
```

- **Core** — `alacritty_terminal`. Adopted, not written (see
  [decisions.md](decisions.md)).
- **Model** — the semantic layer over the grid. The component no other terminal
  has, and where terminite's design effort goes.
- **Multiplexer** — sessions, windows, panes; persistent, detachable. Shaped by
  tmux (see Influences). It is also what makes "many agent sessions at once"
  tractable.
- **Renderer** — `wgpu` drawing of the grid and the Model.
- **Shell** — the OS window and terminite's own crafted, custom-drawn chrome
  (the #2 UI approach; see [decisions.md](decisions.md)).

## The module protocol — where the references converge

terminite has three reference points, and they point at the same mechanism:

- **VS Code** — a thin core; capability is *added* by extensions that are clients
  of a defined API.
- **tmux** — *control mode* (`tmux -CC`) already proved a terminal can be fully
  driven and observed through a structured, machine-readable protocol: tmux emits
  `%`-prefixed event notifications and accepts commands back. iTerm2 renders
  tmux's panes as native UI through exactly this.
- **The thesis** — the AI must read and drive the terminal as cleanly as the
  human.

So terminite's modularity is not a plugin API bolted onto the side. It is a
**protocol that exposes the Model** — and modules *and* the AI are both clients
of it. tmux's control mode is the proof the shape works; terminite generalises
it from a special mode into the terminal's primary extension surface.

Decided: the protocol is **out-of-process** — modules run as separate processes
and speak the protocol over a socket. Language-agnostic, crash-isolated, and the
AI is a client of the same surface as any module. The transport can be
implemented incrementally (simple first, the socket later); the protocol's
*shape* is fixed from the start. See [decisions.md](decisions.md).

## Influences

- **Ghostty** — the loveliness bar.
- **Zed** — the recipe and the ambition: Rust + GPU + a crafted custom-drawn UI.
- **Alacritty** — the low-level stack (an OS window + GPU + `alacritty_terminal`)
  and the proven VT engine.
- **tmux** — the client-server multiplexer model, persistent sessions, and
  control mode as a machine-readable protocol.
- **VS Code** — thin core; modularity as the way capability is added.
