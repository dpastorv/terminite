# Nice-to-haves

Things that improve terminite's UX *if present* but aren't required.
Each one is opt-in on the user's side — terminite still works without
them. The bar for adding a new entry here: if it's not opt-in, it's a
dependency, not a nice-to-have.

## Shell integration (OSC 133 marks)

Required for the block model to populate from real prompts. Without it,
the block gutter stays empty unless you fire marks manually via the
`tools/blocks_demo.sh` script.

terminite has an installer:

```sh
terminite shell-init --install            # auto-detects zsh / bash
terminite shell-init --shell bash --install
```

It writes an idempotent block (between `# >>> terminite shell
integration >>>` and `# <<<` markers) into `~/.zshrc` or `~/.bashrc`.
Re-running replaces only the marked block; everything else in your rc
is left alone.

If you prefer to keep the integration out-of-tree, drive it from your
rc instead:

```zsh
# ~/.zshrc
eval "$(terminite shell-init)"
```

After installation, open a new shell (or `source ~/.zshrc`) and every
command becomes a block (`B1`, `B2`, …) in the gutter. `terminite watch`
streams `block_opened` / `block_closed` events the AI partner can
subscribe to.

## Recommended fonts

terminite's built-in default is the system monospace — works
everywhere, no install. For a denser look you might prefer:

- **JetBrains Mono.** Free, ligatures off looks great in a terminal.
- **Berkeley Mono.** Paid, very tight, very legible.

Set via `font_family = "JetBrains Mono"` in `~/.config/terminite/config.toml`.
Startup-applied — relaunch to change.

## `jq` for pretty proto output

The CLI prints raw JSON. Pipe through `jq` for pretty output:

```
terminite tabs | jq .
terminite watch | jq -c .
```

Optional everywhere; not assumed.
