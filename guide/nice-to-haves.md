# Nice-to-haves

Things that improve terminite's UX *if present* but aren't required.
Each one is opt-in on the user's side — terminite still works without
them. The bar for adding a new entry here: if it's not opt-in, it's a
dependency, not a nice-to-have.

## Shell integration (OSC 133 marks)

Required for the block model to populate from real prompts. Without it,
the block gutter stays empty unless you fire marks manually via the
`tools/blocks_demo.sh` script.

**zsh** — add to `~/.zshrc`:

```zsh
preexec() { printf '\e]133;C\e\\' }
precmd() {
  local code=$?
  printf '\e]133;D;%d\e\\' "$code"
  printf '\e]133;A\e\\'
}
```

**bash** — `PROMPT_COMMAND` analogue:

```bash
__terminite_preexec() { printf '\e]133;C\e\\'; }
__terminite_precmd() {
  local code=$?
  printf '\e]133;D;%d\e\\' "$code"
  printf '\e]133;A\e\\'
}
trap '__terminite_preexec' DEBUG
PROMPT_COMMAND='__terminite_precmd'
```

After this, every command run in the shell auto-populates a block in
terminite's gutter (`B1`, `B2`, …), and `terminite watch` streams
`block_opened` / `block_closed` events the AI partner can subscribe to.

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
