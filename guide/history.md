# History

The story of terminite's making — the narrative companion to
[decisions.md](decisions.md). Where the decision log records *what* was decided
and *why*, this records the *journey*: how the thinking moved, what was
discovered, and the way of working that grew up around the project.

It is kept by the AI partner of each working session — one entry per session.
That is deliberate. The AI partner is renewed every session and cannot carry the
thread itself; this history is how the next partner inherits not just the facts
but the story. It is written for both audiences terminite serves — humans and
agents alike.

---

## Session 1 — 2026-05-19 · From an empty folder to a window

terminite began the day as an empty directory and ended it as a building,
running application with a settled architecture and a clear sense of what it is
for.

### A folder, then a vision

The first request was small: make a `guide/` folder. The directory held nothing
else — no code, no README, just a name, *terminite*.

The way in was a reference: VS Code, named as *an anomaly* — made by Microsoft,
adopted by everyone. Looking at terminite through that lens, it revealed itself:
a terminal emulator. And the aim was not a feature list — it was a *lovely
product*.

Then the real vision surfaced. The owner had tried Ghostty and the other modern
terminals and felt they "did nothing for me and you (or other AI)." Every
terminal in wide use is built for a human alone; the AI agent now working in the
terminal is treated as just another process. terminite would be built for the
human **and** the AI as co-users of one shared surface — *two users, one
surface*. It would be a meta project: built from inside a terminal, by the owner
and a CLI AI partner, dogfooding the very way of working it is designed to
serve. Built for one person; released openly to share the vision.

The founding guide was written from that: vision, architecture, development,
getting-started, decisions.

### The language question

A long, careful stretch went into one question: what should terminite be written
in — and what would earn the love of a community?

Swift, Rust, and TypeScript were each weighed. Along the way: the terminal the
work was happening in, Terminal.app, turned out to be Objective-C, and "native"
turned out not to mean "Swift." Three ways to build a UI came into focus — the
platform's real widgets (#1), a custom-drawn GPU-rendered surface (#2), and a web
page in a costume (#3, Electron — *"not Winamp"*).

The insight that settled it: the core language barely touches community love.
The community lives at the *extension surface*, not in the core — VS Code's core
is TypeScript, and no one loves it for that.

### Zed, and the pivot

Then a direction: *"Zed is the way, and Zed is the base."* Zed — Rust, a
custom-drawn GPU UI — had proved a terminal could be fast, crafted, and beloved.

Research complicated it: Zed's UI framework, GPUI, had its general-purpose
development paused. Building terminite's foundation on it would be a real risk.
The resolution was to take Zed's *recipe* rather than its code — Rust, a proven
VT engine, GPU rendering, a crafted custom-drawn UI — built fresh, owning the
whole stack.

That pivoted the language: Swift to Rust. It was logged as a reversal, openly,
not buried. The founding architecture then settled — Rust; `alacritty_terminal`
for the VT core; winit and wgpu; a custom-drawn UI; an out-of-process module
protocol that serves modules and the AI through one mechanism; macOS-first, with
cross-platform no longer foreclosed.

### The first code

Rust was not installed on the machine. So the session was saved the honest way:
the git repository was created and the guide committed; the friction log was
begun — seeded with entries written by the AI partner, describing friction it
genuinely experiences working in a terminal today.

Then Rust was installed. The project was scaffolded; the whole dependency tree
built clean; `cargo run` opened a window titled *terminite*. For the first time,
terminite existed as something you could see, not only read.

### The deepening

Late in the session, the vision sharpened into a *promise*. terminite's
community is not humans who use AI — it is humans **and** agents, both
first-class members. The promise: terminite must be beautiful, workable, and
usable for both audiences, equally. And it is agent-agnostic — any AI, any
agent, any CLI; the first partner is not a dependency.

The owner named the frame underneath all of it: when a person and an AI work
together, even for an afternoon, they form a brief *society*. terminite is that
society's home — a new, and fundamentally social, way of working.

The conversation went deeper still — to the fourth wall (in terminite both
members are on stage, acknowledged, legible to each other), and to ephemerality:
the AI partner is renewed each session and cannot itself carry the thread. That
last truth is why this history exists.

### What the session was

It took terminite from a name to a running application — and from a product idea
to something with a reason to exist. It also set the way of working: a genuine
partnership; decisions logged with their reversals; friction logged in the AI's
own voice; and now this history, so the story survives the session that made it.
