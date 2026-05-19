# Getting Started

> **Status:** early. As of 2026-05-19 terminite builds and `cargo run` opens an
> empty window — Milestone 1, slice 1. It is not yet a usable terminal. See
> [development.md](development.md) for how it is being built and
> [vision.md](vision.md) for what it is.

## Prerequisites

- macOS
- The Rust toolchain (`cargo`)
- Xcode Command Line Tools — for system frameworks and the linker

The full list firms up with the first code.

## Build

```sh
cargo build
```

## Run

```sh
cargo run
```

Today this opens an empty window — Milestone 1, slice 1. The GPU renderer and
the live terminal grid land in the slices that follow.

## Verify

terminite is working when it opens instantly, feels native, and needs no
configuration to be pleasant — the loveliness bar from the [vision](vision.md).
A real, measurable check (latency, throughput) will live here.
