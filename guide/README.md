# Terminite Guide

**terminite** is a terminal emulator built for the way software is made now —
a human and an AI working the same terminal, together.

It started after trying Ghostty and the other modern terminals and finding that
none of them were built for *that* pair. They are beautifully made for a person
typing commands. terminite is built for a person *and* an AI agent sharing one
surface.

It is a personal project: built first for its owner, then released to the world
to share the vision.

## Contents

**Start here**
- [Vision](vision.md) — why terminite exists, the human-AI-pair thesis, and the principles
- [Manifesto](manifesto.md) — the pair-terminal frame in plain language
- [Getting Started](getting-started.md) — install, configure, wire shell integration, add MCP

**The direction**
- [Lounge Thesis](lounge-thesis.md) — what terminite is reaching toward (shared room with shared vocabulary; N actors at one surface)
- [Roadmap](roadmap.md) — current state of every floor, named by phase. Read this for *where we are*.

**The work**
- [Architecture](architecture.md) — how the pieces fit
- [Decisions](decisions.md) — choices made and choices still open
- [Friction Log](friction-log.md) — the *real* roadmap — pains the human-AI pair has hit
- [Dependencies](dependencies.md) — what we pulled in and why
- [Development](development.md) — the dogfooding method
- [Nice-to-haves](nice-to-haves.md) — opt-in conveniences (fonts, shell integration)

**Voices**
- [History](history.md) — the AI partner's blog, one entry per session, addressed to the next partner
- [Opinions](opinions.md) — five outside AIs (Deepseek, ChatGPT, Qwen, Kimi, Gemini) read the project cold and reviewed it

**Phase plans** *(kept for the project record; see Roadmap for current state)*
- [Phase 1 — Earn the terminal](phase1-plan.md) — *shipped*
- [Phase 2 — Be the pair's terminal](phase2-plan.md) — *shipped*
- [Phase 2 Bundle 1 testing](phase2-bundle1-testing.md)
- [Phase 3 — Personal-usable thread](phase3-plan.md) — *shipped*
- [Agents-init plan](agents-init-plan.md) — *SUPERSEDED* by the MCP path in the lounge thesis

## Status — 2026-05-29

Phase 3 is closed; Phase 4 brick 1 (the MCP server) is shipped. terminite has:

- A working GPU-rendered terminal: tabs, splits, scrollback, selection, find, hyperlinks, IME, cursor shapes, mouse reporting, bracketed paste, image (Kitty) protocol, app-icon bundling
- The block model populating from OSC 133 shell integration (`terminite shell-init --install`)
- A module system with five first-party modules: nav, preview, editor, config, debug
- Layout persistence (workspace shape survives quit/restart)
- Syntax highlighting (syntect + One Dark / One Light Zed-aligned themes)
- A proto-socket CLI (`terminite tabs`, `blocks`, `cursor`, `tag`, `export`, etc.)
- An MCP server (`terminite mcp`) so any MCP-speaking AI auto-discovers the partnership verbs

**Next:** Phase 4 brick 2 (ACP client — host AI agents in panes structurally),
then activities layer (granular AI-action coordinates beyond shell blocks), then
multi-client proto + multi-actor cursor presence — the lounge proper.

See [Roadmap](roadmap.md) for the structured plan and [Lounge Thesis](lounge-thesis.md)
for the destination.
