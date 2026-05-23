# Dependencies

Tracked deliberately. Each one has to argue its way in, and the list is
audited any time we add a piece. The bar: lean *and* complete — terminite
owns its UX end-to-end. We don't depend on third-party tools to use our
own terminal.

## Hard dependencies (Cargo)

Crates terminite needs to build and run. Vendored crates carry their own
patches and travel with the repo.

| Crate | What it does | Why |
|---|---|---|
| `alacritty_terminal` (**vendored**) | VT parser, grid, scrollback. | The core terminal emulation. Vendored under `vendor/` so terminite owns its OSC 7 + OSC 133 + APC patches. |
| `vte` (**vendored, transitive**) | ANSI escape parser. | Same fork — added the APC string state. |
| `arboard` | System clipboard. | Cmd-C / Cmd-V across platforms. |
| `base64` | Base64 codec. | Decoding Kitty graphics payloads. |
| `bytemuck` | `Pod` / `Zeroable`. | wgpu vertex / uniform buffer casts. |
| `glyphon` + `cosmic-text` | Glyph shaping + GPU text. | Text rendering. The terminal's text path. |
| `libc` | POSIX bindings. | RSS kill switch, socket modes. |
| `png` | PNG decoder. | Kitty graphics PNG payloads. |
| `pollster` | Synchronous `await`. | One-shot wgpu adapter request at startup. |
| `serde` + `serde_json` | Serialization. | Module-protocol wire format. |
| `wgpu` | GPU API. | The render pipeline. |
| `winit` | Windowing + input. | Window, keyboard, mouse, focus events. |

## System / external tools

**None required.** terminite is self-contained. The binary is the
window, *and* the CLI for talking to the window's module protocol:

```
terminite                          launch the window
terminite tabs                     list open tabs
terminite blocks [tab_id]          list blocks
terminite block <tab> <id>         print one block's command + output
terminite watch                    stream block events
```

No `socat`, `nc`, `jq`, `python` required to use the protocol. Pipe to
`jq` if you want pretty JSON — it's not assumed.

## Deliberate non-deps

Things terminite *could* depend on but doesn't, by design.

- **TOML crate.** Config is a handful of scalar `key = value` lines —
  the hand-rolled parser in `src/config.rs` is ~50 lines and has no
  surface area for surprises. If config grows tables or arrays this
  flips.
- **clap / structopt.** The CLI has four subcommands and no flags;
  hand-rolled dispatch in `src/proto_client.rs` is clearer than a
  derive macro for this surface size.
- **tokio / async-std.** The render loop is synchronous winit; the
  module-protocol server uses two threads per connection. Standard
  library is enough. Adopting async would be a deliberate phase
  transition, not a creeping convenience.
- **tracing / log.** `eprintln!` covers the operational logging we have.
  Add structured logging when we have a reader for it.

## Adding a new dep

Before adding, answer:

1. What does it do that we can't write in <100 lines?
2. Does it pull in *its* transitive deps we don't want?
3. Does it change a deliberate non-dep stance above?
4. Could a hand-rolled version live in `src/` instead?

A "yes I can write this trivially" usually means we should — keeps the
crate tree shallow and the binary small.
