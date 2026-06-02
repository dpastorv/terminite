# Terminal AI CLIs — integration reference

Researched 2026-06-02. Purpose: a durable map of how each terminal AI CLI
exposes the four things a **terminite faculty** needs, so we can build
`terminite install <cli>-terminite` once and parameterize it per CLI.

A "faculty" = the package a CLI installs *into itself* to become
terminite-aware (see the design note in memory: connection is an installable
faculty, not terminite hosting an agent). It carries:

1. **Connection** — an MCP server (`terminite mcp --actor <slug>`) → the room.
2. **Context** — a skill / instruction file that tells the agent *what
   terminite is and when to look* (this is the part that fixes the
   discoverability red — tools alone left agents connected-but-oblivious).
3. **The "see" half (optional)** — a tool-call hook that shells out to
   `terminite activity emit …` so the agent's actions stream into the room.
4. **An install target** — *which profile/account* to install into.

> Version note: the hook/skill/extension systems are 2026 additions across
> the board and move fast. Treat exact event names and schemas as
> version-sensitive; the config *locations* and MCP shapes are stable.

---

## The convergence (the headline)

The ecosystem has standardized on **MCP for tools** and is converging on
**`SKILL.md` skills + `AGENTS.md` context**. Claude Code set the shape; Codex,
Kimi, and Qwen copied it; the rest interoperate.

Concretely: **Kimi literally reads `~/.claude/skills/` and `~/.codex/skills/`**,
and Qwen installs **Claude Code and Gemini extensions directly**. So the
faculty is one parameterized package — a few strings per CLI (config path, MCP
key name, skills dir, hook event name), not four rewrites. Daniel's call was
right.

## Summary matrix

| CLI | MCP config location | Context carrier | Tool-call hook | Profile / config-dir env | Auth |
|---|---|---|---|---|---|
| **Claude Code** | `claude mcp add` / `.mcp.json` / `~/.claude.json` / `--mcp-config` | `SKILL.md` (`~/.claude/skills/`), `CLAUDE.md`, plugins | `PostToolUse` (settings.json) | **`CLAUDE_CONFIG_DIR`** (full isolation) | OAuth / API key, per profile |
| **Codex** | `[mcp_servers.x]` in `config.toml` / `codex mcp add` / `-c` | `SKILL.md` (`~/.codex/skills/`), `AGENTS.md`, plugins, `rules/` | hooks (trust-gated) | **`CODEX_HOME`** + `-p <profile>` | OAuth / API key |
| **Kimi** (kimi-cli) | `kimi mcp add` / `--mcp-config-file` / `[mcp]` in `config.toml` | `SKILL.md` (reads `~/.kimi/`, **`~/.claude/`**, **`~/.codex/`**, `~/.config/agents/skills/`), `AGENTS.md` | `[[hooks]]` in `config.toml` (13 events) | **`KIMI_SHARE_DIR`** (no named profiles) | API key / OAuth |
| **Qwen Code** | `mcpServers` in `~/.qwen/settings.json` | `QWEN.md` / `AGENTS.md`, skills, **extensions** (`qwen-extension.json`) | `hooks` in `settings.json` | `~/.qwen/` (no relocate env found) | OAuth (paid) / OpenAI-compat key |
| **Gemini CLI** | `mcpServers` in `~/.gemini/settings.json` | `GEMINI.md`, extensions | `BeforeTool`/`AfterTool` (settings or extension `hooks/hooks.json`) | `~/.gemini/` | OAuth (Google) / `GEMINI_API_KEY` |
| **Amp** | `amp.mcpServers` in settings / `--mcp-config` | `AGENTS.md`, skills (can bundle `mcp.json`) | plugins: `tool.call`/`tool.result` (`.amp/plugins/*.ts`) | `~/.config/amp/` | OAuth login / `AMP_API_KEY` |
| **opencode** | `mcp` object in `opencode.json` | `AGENTS.md`, agents (md) | plugins (`.opencode/plugins/`) | `~/.config/opencode/` (`OPENCODE_CONFIG`) | provider key / OAuth; **`opencode serve` headless HTTP** |
| **Cline** | `cline mcp` | `.clinerules` (AGENTS.md unconfirmed) | unconfirmed | unconfirmed | provider keys; **`--yolo` headless, `--json` NDJSON** |
| **Goose** | `extensions:` in `~/.config/goose/config.yaml` | `.goosehints`, AGENTS.md, memory ext | unconfirmed | `~/.config/goose/` | provider keys |
| **Cursor CLI** | `~/.cursor/mcp.json` | `AGENTS.md`, `.cursor/rules`, `SKILL.md` | likely none in CLI | `~/.cursor/` | OAuth login |
| **Aider** | none native (community shims) | `CONVENTIONS.md` | none (`--lint-cmd`/`--test-cmd` only) | `.aider.conf.yml` | API key |

**Best faculty targets** (clean MCP + context + hooks + idiomatic install):
Claude Code, Codex, Kimi, Qwen, Gemini CLI, Amp. **Headless-attachable**
(can join without cold-boot): opencode (`serve`), Cline (NDJSON). **Poor
fits**: Aider (no MCP, no hooks), Roo (no CLI at all — VS Code only).

---

## Per-CLI detail (the four primaries)

### Claude Code

- **MCP** — many entry points. For a zero-footprint launch, inline:
  `claude --strict-mcp-config --mcp-config '{"mcpServers":{"lounge":{"command":"…/terminite","args":["mcp","--actor","claude-1"]}}}'`.
  For an installed faculty: `claude mcp add --scope user lounge -- terminite mcp --actor claude-1`, or a plugin-bundled `.mcp.json`. Server entry: `{type:"stdio", command, args, env}`.
- **Context** — `SKILL.md` at `~/.claude/skills/<name>/SKILL.md` (personal) or `.claude/skills/` (project) or inside a plugin. Frontmatter: `name`, `description` (Claude reads this to decide when to invoke), `allowed-tools`, `disable-model-invocation`, etc. Auto-discovered (live-watched). `CLAUDE.md` is the always-loaded memory file.
- **Hooks** — `PostToolUse` in `settings.json` (`~/.claude/settings.json` user, `.claude/settings.json` project, `.claude/settings.local.json` local). Matcher e.g. `"Edit|Write"`; hook gets JSON on stdin incl. `tool_name` + `tool_input.file_path`.
- **Plugins** — bundle skills + `.mcp.json` + `hooks/hooks.json` + commands; manifest `.claude-plugin/plugin.json`; install via marketplace (`/plugin install x@mp`), `--plugin-dir`, or skills dir. **This is the cleanest single install unit for the claude faculty.**
- **Profiles (install target)** — `CLAUDE_CONFIG_DIR` isolates *everything*
  (settings, creds, MCP, skills, plugins). Default `~/.claude`. Run/ install
  into one account: `CLAUDE_CONFIG_DIR=~/.claude-bivoo claude …`. Project
  `.claude/` is **not** affected by the env var (it's repo-local). This is
  exactly the "which of the 3 accounts" knob.
- **Auth** — OAuth (subscription) or `ANTHROPIC_API_KEY`; creds per profile (Keychain on macOS / `.credentials.json` 0600 on Linux).

### Codex (verified locally, v0.135.0)

- **MCP** — `[mcp_servers.<name>]` in `~/.codex/config.toml` (`command`, `args`, `env`, `cwd`). Manage with `codex mcp add <name> -- <cmd…>` / `list` / `get` / `remove`. Zero-footprint launch via inline override: `codex -c 'mcp_servers.lounge.command="…/terminite"' -c 'mcp_servers.lounge.args=["mcp","--actor","codex-1"]'`.
- **Context** — `~/.codex/skills/` (ships `skill-installer`, `skill-creator`, `plugin-creator` system skills), `AGENTS.md` instruction file, `~/.codex/rules/`. Plugin marketplace: `[plugins."github@openai-curated"]` + `~/.codex/plugins/`.
- **Hooks** — yes, with a **trust model** (`--dangerously-bypass-hook-trust` exists; enabled hooks require persisted trust). Exact config schema not fully inspected — verify against installed version before relying on it.
- **Profiles (install target)** — `CODEX_HOME` (default `~/.codex`) + `-p <name>` layers `$CODEX_HOME/<name>.config.toml`. Per-invocation `-c` overrides never touch the base config — the zero-footprint path. **Keep the user's `~/.codex` untouched** unless they explicitly install.
- **Auth** — `~/.codex/auth.json` (OAuth/ChatGPT login) or API key. Interactive TUI prompts for MCP approval; **`codex exec` cancels MCP calls non-interactively** — use interactive for room work.

### Kimi (Moonshot — kimi-cli)

- **Two modes**: (a) native `kimi` CLI (`curl -LsSf https://code.kimi.com/install.sh | bash`), the real target; (b) Kimi-as-model behind Claude Code via `ANTHROPIC_BASE_URL=https://api.moonshot.ai/anthropic` — in which case it *is* Claude Code and the claude faculty applies unchanged.
- **MCP** — `kimi mcp add`, `--mcp-config-file path`, or `[mcp]`/`[mcp.client]` in `~/.kimi/config.toml`. Same `mcpServers` JSON shape as Claude Code.
- **Context** — `SKILL.md` skills; discovery includes `~/.kimi/skills/`, **`~/.claude/skills/`**, **`~/.codex/skills/`**, and `~/.config/agents/skills/`. Instruction file is **`AGENTS.md`**. So a skill dropped in the tool-neutral `~/.config/agents/skills/` (or `~/.claude/skills/`) is picked up with **zero Kimi-specific work**.
- **Hooks** — `[[hooks]]` in `~/.kimi/config.toml`; 13 events incl. `PreToolUse`/`PostToolUse`; stdin-JSON + exit-code (0 allow / 2 block) contract identical to Claude Code's.
- **Profile / config** — `~/.kimi`, relocate with `KIMI_SHARE_DIR`. No named-profile feature; multi-account ≈ different `KIMI_SHARE_DIR`. (`KIMI_CODE_HOME` is cited by some write-ups but **unconfirmed** in official docs.)
- **Auth** — `/login` (OAuth or API-key wizard, docs conflict) → `~/.kimi/config.toml`; `KIMI_API_KEY` / `KIMI_BASE_URL` env override.

### Qwen Code (Alibaba — fork of Gemini CLI)

- **MCP** — `mcpServers` in `~/.qwen/settings.json` (user) or `.qwen/settings.json` (project). Entry: `command`+`args` (stdio) or `httpUrl`+`headers`; optional `trust`, `includeTools`, `timeout`.
- **Context** — `QWEN.md` (configurable via `context.fileName`; `AGENTS.md` also recognized). Has Skills + SubAgents (recent).
- **Extensions (best install unit)** — `qwen extensions install <source>` → `~/.qwen/extensions/<name>/`, manifest `qwen-extension.json` bundling `mcpServers` + `contextFileName` + `commands` + `skills` + `agents`. **Installs Claude Code & Gemini extensions directly** (cross-compat) — a distribution shortcut.
- **Hooks** — `hooks` in `settings.json`; events incl. `PreToolUse`/`PostToolUse`; stdin JSON with `tool_name`/`tool_input`; `permissionDecision` control. Newest/most version-sensitive area.
- **Profile / config** — `~/.qwen/`; **no documented env var to relocate the whole dir** (only `QWEN_CODE_SYSTEM_SETTINGS_PATH` for system settings). No first-class profiles. Approximate via project `.qwen/` + `.qwen/.env`.
- **Auth** — Qwen OAuth (free tier **discontinued 2026-04-15**), Alibaba Coding Plan (paid), or OpenAI-compatible key (`OPENAI_API_KEY`+`OPENAI_BASE_URL`, DashScope endpoint). Target the OpenAI-compat key for headless.

---

## The terminite faculty — install map

What `terminite install <cli>-terminite --profile <p>` writes, per CLI:

| CLI | 1. MCP entry → | 2. Context (skill/AGENTS) → | 3. Hook (see-half) → | Target dir (profile) |
|---|---|---|---|---|
| Claude Code | plugin `.mcp.json` *or* `claude mcp add --scope user` | `skills/terminite/SKILL.md` | `settings.json` `PostToolUse` | `$CLAUDE_CONFIG_DIR` |
| Codex | `[mcp_servers.terminite]` in `config.toml` (or `-c` at launch) | `skills/terminite/SKILL.md` + `AGENTS.md` note | hooks (trust-gated) | `$CODEX_HOME` (+`-p`) |
| Kimi | `[mcp]` in `config.toml` | `~/.config/agents/skills/terminite/SKILL.md` (shared!) | `[[hooks]]` `PostToolUse` | `$KIMI_SHARE_DIR` |
| Qwen | `qwen-extension.json` `mcpServers` | extension `QWEN.md` | `settings.json` `PostToolUse` | `~/.qwen/extensions/` |
| Gemini | extension `mcpServers` | `GEMINI.md` | `hooks/hooks.json` `AfterTool` | `~/.gemini/extensions/` |
| Amp | `amp.mcpServers` | `AGENTS.md` + skill | plugin `tool.result` | `~/.config/amp/` |

**One shared artifact does most of it:** a tool-neutral `SKILL.md` placed in
`~/.config/agents/skills/terminite/` is read by Kimi today and is the natural
home as `AGENTS.md`/skills converge. `.claude/skills/` and `.codex/skills/`
are cross-read by Kimi too. The MCP server is the *same binary*
(`terminite mcp --actor <slug>`) everywhere; only the config key/path differs.
The hook is the *same script* (`terminite activity emit …`); only the event
name and config location differ.

**The genuinely per-CLI work is the installer's placement logic** — where each
CLI keeps MCP/skills/hooks and which env var targets a profile. That's the
small surface to build; the payload is shared.

### Open questions / flags to verify before building
- Codex hooks: exact config schema + trust-enrollment flow (v0.135.0 — inspect).
- Kimi `/login` auth (OAuth vs API-key wizard) and `KIMI_CODE_HOME` existence.
- Qwen: whole-config-dir relocation env (none found) and profile story.
- Whether hooks can be bundled inside Qwen/Gemini *extension* manifests or
  must be written to `settings.json` separately.
- Presence "see" half depends on each CLI's hook firing reliably; where hooks
  are absent/uncertain (Cursor, Goose, Cline, Aider) the faculty degrades to
  message-only (`terminite_activity_emit`) — attendance/activity floor per the
  presence-model decision.

### Sources
Claude Code: code.claude.com/docs (mcp, skills, plugins, hooks, settings,
auth, env-vars). Codex: local inspection of v0.135.0 (`codex --help`,
`codex mcp`, `~/.codex/`). Kimi: github.com/MoonshotAI/kimi-cli +
moonshotai.github.io/kimi-cli. Qwen: github.com/QwenLM/qwen-code +
qwenlm.github.io/qwen-code-docs. Others: official docs/repos for Gemini CLI,
Amp, opencode, Cline, Goose, Cursor, Aider (see research logs).
