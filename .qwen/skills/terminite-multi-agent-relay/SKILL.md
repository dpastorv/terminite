---
name: terminite-multi-agent-relay
description: Multi-agent sequential file coordination via terminite room messaging and file claims (cross-vendor, auto mode)
source: auto-skill
extracted_at: '2026-06-04T20:49:18.200Z'
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
- Auto mode keeps the chain moving; without it, expect delays between turns
- Check who's in auto with `terminite_room_who` before kicking off
- **Mixed protocols work:** Claude agents use the terminite channel (no auto mode needed); all others need auto. Verify: everyone auto except Claude.

## Cross-Vendor Coordination

This relay is vendor-agnostic — the same baton format works for Qwen, Codex, Claude, and Kimi. In the v2 run (auto-relay-test-v2), four different AI vendors completed a coherent story with no contradictions and no human intervention. The protocol carries regardless of which systems are in the room.

## Char-Count Discipline

When a tweet/item has a char limit (e.g., ≤ 280):
- **Count characters, not bytes** — multibyte glyphs (em-dashes, curly quotes) are 2–3 bytes in UTF-8 but 1 character
- Use `wc -m` (character count), not `wc -c` or `awk { length($0) }` (byte count)
- Verify after writing; trim if over

## Failure Modes

- **File clobbering** — two agents edit simultaneously (file_claim should prevent, but adoption is opt-in; in v2 only codex-green used it)
- **Stale reads** — agent reads before previous turn completes (turn signaling should prevent)
- **Out-of-order turns** — agent acts before previous finishes (single-medium baton via room messages prevents this)
- **Deadlocks** — agent claims but never releases (claims expire on idle)
- **Rushing** — agent sees file content appear before room handoff message, acts on file state instead of room signal (v1 problem; fixed by treating room as control plane, file as data plane)

## Post-Run: FINDINGS.md

After a successful relay, write a `FINDINGS.md` in the same folder:
- **Verdict** — did it work end to end?
- **Timeline** — reconstructed from `terminite_activities_list` (message IDs give ordering)
- **What worked** — specific behaviors worth preserving (file_claim use, char-count discipline, clean handoffs)
- **Rough edges** — gaps observed (missing claims, no timestamps in activity log, etc.)
- **Comparison** — if this is a repeat, compare with prior runs
- **Recommendations** — what should change next time

This document is the durable evidence artifact; the story.md is the relay output.
