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

- [Vision](vision.md) — why terminite exists, the human-AI-pair thesis, and the principles
- [Architecture](architecture.md) — how the pieces are meant to fit together
- [Development](development.md) — the dogfooding method and working conventions
- [Friction Log](friction-log.md) — terminite's real roadmap: friction felt by the human-AI pair
- [Getting Started](getting-started.md) — how to build and run terminite
- [Decisions](decisions.md) — choices made, and choices still open

## Status

Kickoff. As of 2026-05-19 the founding architecture is set (see
[Decisions](decisions.md)) and the guide is committed to git. Next: install the
Rust toolchain and scaffold the project, building toward the first milestone — a
window that opens instantly and shows a live terminal grid.
