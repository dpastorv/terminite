# Getting Started

terminite is a GPU-rendered terminal for the human–AI pair. It's a real
working terminal with panes, tabs, scrollback, and shell-integrated
**blocks** — every command's prompt + output is a structured unit the
pair can name, tag, and reference. An out-of-process AI partner can
read the block model and write back over a Unix socket; you can drive
the same surface from the CLI.

## Prerequisites

- macOS (primary target). Linux likely works but isn't dogfooded.
- The Rust toolchain — `cargo`. Install via `rustup` if you don't have it.
- Xcode Command Line Tools (`xcode-select --install`) — for the linker
  and system frameworks.

No system tools like `socat` / `jq` / `python` are required to use
terminite. The `terminite` binary is both the window and its own CLI.

## Build

From the project root:

```sh
cargo build              # debug
cargo build --release    # optimized; what you'll want once you're using it
```

## Install (recommended)

Put `terminite` on PATH so the CLI subcommands work from any directory:

```sh
cargo install --path .
```

This installs a release build to `~/.cargo/bin/terminite`. Re-run after
pulling new commits to pick up changes.

Without installing, you can still run everything via
`./target/debug/terminite` or `./target/release/terminite`.

## Run

```sh
terminite        # launches the window
```

That's the whole window app. New tabs: Cmd+T. Close tab/pane: Cmd+W.
Split pane: Cmd+D (vertical) / Cmd+Shift+D (horizontal). Drag any
border between panes to resize. Cmd-drag a pane's corner handle to
split. Cmd+1…Cmd+9 jumps to tab by index.

## Configure

User config lives at `~/.config/terminite/config.toml`. Copy the example
to start:

```sh
mkdir -p ~/.config/terminite
cp guide/config.example.toml ~/.config/terminite/config.toml
```

Most fields **hot-reload on focus**: edit the file in a side pane,
click back into a shell pane, the values apply. See
[config.example.toml](config.example.toml) for the full list with
inline docs (padding, gutter, line height, highlight, cursor blink,
bell, scrollback).

## Shell integration (recommended)

The block model populates from OSC 133 marks that your shell needs to
emit. Without them, the gutter stays empty and the AI partner can't
see structured blocks. **zsh**, add to `~/.zshrc`:

```zsh
preexec() { printf '\e]133;C\e\\' }
precmd() {
  local code=$?
  printf '\e]133;D;%d\e\\' "$code"
  printf '\e]133;A\e\\'
}
```

For **bash** + other shells, see [nice-to-haves.md](nice-to-haves.md).
After this, every command run in the shell becomes a labeled block
(`B1`, `B2`, …) in terminite's left gutter.

## The CLI

The same binary doubles as a CLI client for the running terminite
window — reads the block model, writes tags + AI cursor.

```sh
terminite                          launch the window
terminite tabs                     list open tabs
terminite blocks [tab_id]          list blocks in a tab (default 0)
terminite block <tab> <id>         print one block's command + output
terminite watch                    stream block_opened / block_closed events
terminite tag <tab> <id> <tag>     attach a tag to a block (gutter highlight)
terminite untag <tab> <id> <tag>   remove a tag
terminite cursor <tab> <id>        move the AI cursor (warm amber highlight)
terminite cursor-clear <tab>       drop the AI cursor
terminite help                     usage
```

Pipe to `jq` for pretty output (`terminite tabs | jq .`). Raw is one
JSON line per response, suitable for scripting.

The socket is at `~/.terminite/socket` (override with
`$TERMINITE_SOCKET`). Anything that speaks Unix sockets can connect —
see [architecture.md](architecture.md) for the protocol shape.

## A 30-second tour

In a fresh terminite with shell integration wired up:

```sh
cd ~/some/project
ls              # produces B1
cargo test      # produces B2

# In a side pane (or another terminal):
terminite blocks 0
terminite block 0 1                       # see B1's output, structured
terminite tag 0 2 the-failing-test        # B2 gets a subtle highlight
terminite cursor 0 2                      # B2 goes warm-amber — "AI is here"
terminite watch                           # live event stream while you keep working
```

That's the partnership surface — both halves of the pair pointing at
the same `B2`, in the same coordinate system, in real time.

## Where to next

- [vision.md](vision.md) — what terminite is for.
- [manifesto.md](manifesto.md) — the partnership principle.
- [architecture.md](architecture.md) — how the pieces fit.
- [phase2-plan.md](phase2-plan.md) — what's still landing.
- [dependencies.md](dependencies.md) — what terminite is built from, and isn't.
- [nice-to-haves.md](nice-to-haves.md) — opt-in things that improve the UX.
- [decisions.md](decisions.md) — why the calls were made.
