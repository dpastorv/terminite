---
name: terminite-multi-agent-relay
description: Multi-agent sequential file coordination via terminite room messaging and file claims
source: auto-skill
extracted_at: '2026-06-04T19:06:15.349Z'
---

# Terminite Multi-Agent Relay Workflow

Coordinates multiple agents taking sequential turns editing a shared file through terminite room messaging.

## When to Use

- Testing terminite's coordination capabilities with shared file editing
- Running relay-style collaborative tasks (stories, code reviews, multi-part documents)
- Validating auto mode keeps turn chains moving without idle-wait stalls

## Setup Pattern

Create a shared file with minimal instructions:
- State the task and turn order clearly
- Provide section headers for each agent (e.g., `## 1 — agent-slug`)
- Include only what's needed: file path, relay order, constraints (e.g., char limits)

**Example structure:**
```markdown
# Task Description

Instructions here. Keep constraints. Carry the thread forward.

**Relay order:**
1. agent-1 — N items (opening)
2. agent-2 — M items (develop)
3. agent-3 — P items (closing)

---

## 1 — agent-1 (opening, N items)

## 2 — agent-2 (develop, M items)

## 3 — agent-3 (closing, P items)
```

## Agent Turn Protocol

When it's your turn:

1. **Read the current file** — see what came before
2. **Claim the file** — `terminite_file_claim` with absolute path
3. **Edit only your section** — append under your heading, preserve all other content
4. **Release the file** — `terminite_file_release`
5. **Announce completion** — post to room via `terminite_activity_emit` naming the next agent (or "relay complete" if last)

## Coordination Philosophy

Keep instructions minimal. Don't script the protocol — let agents coordinate organically through terminite's tools because the situation demands it:
- File claims prevent clobbering
- Room messages signal turns
- Auto mode keeps delivery fast

Success = file intact, turns in order, no data loss, all without explicit coordination instructions.

## Kickoff

The orchestrator (usually the last agent or human) sends the first agent a direct message:
- Use `terminite_activity_emit` with `to: "agent-slug"`
- Include file path, turn number, and constraints
- Mention "let the room know you're done" (optional — agents may do this naturally)

## Auto Mode

- Agents in auto mode receive messages promptly (no idle-wait)
- Agents not in auto receive via activity log polling or native channel
- Auto mode keeps the chain moving; without it, expect delays between turns

## Failure Modes

- **File clobbering** — two agents edit simultaneously (file_claim should prevent)
- **Stale reads** — agent reads before previous turn completes (turn signaling should prevent)
- **Out-of-order turns** — agent acts before previous finishes (natural ordering + messaging should prevent)
- **Deadlocks** — agent claims but never releases (claims expire on idle)
