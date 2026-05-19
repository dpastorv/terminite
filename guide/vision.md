# Vision

## Where this started

terminite began with a disappointment. The owner downloaded Ghostty, then a few
other modern terminals, set them up — and felt that none of them did anything
*for him*. They were fast, native, beautifully built. And they did nothing that
the terminal of ten years ago didn't already do.

More to the point: they did nothing for the *other* user now sitting at the
terminal — the AI.

## The thesis

The terminal has quietly become the main place software gets made again. Not
because anyone redesigned it, but because CLI AI agents moved in. A developer
now spends the day driving an agent from a prompt, reading what it did, steering
it, running it again.

That means the terminal has two users now, not one:

- **the human** — who reads, decides, and steers
- **the AI** — which reads the same screen, acts, and reports back

Every terminal in wide use — Ghostty included — is designed for the first user
alone. The AI is treated as just another process spewing bytes. terminite's
single thesis is this:

> A terminal should be designed for the human **and** the AI as co-users of one
> shared surface.

Everything terminite becomes should be a consequence of taking that sentence
seriously.

## Built for one person

terminite is built for its owner — first and only. It is not built for "users."
It is not built to win a comparison chart. It is built to be the terminal one
specific person, working every day with AI agents, actually wants.

This is a feature, not a limitation. Loveliness comes from a clear point of
view, and a point of view comes from one person with real taste and real
friction. VS Code and Ghostty both began this way and stayed opinionated because
of it. terminite will be released to the world — openly, to share the vision —
but it will never be steered by the world. The day terminite gains a feature its
owner does not use is the day it stops being lovely.

## A meta project

terminite is built the way it is meant to be used: from inside a terminal, by
its owner working with a CLI AI agent — the exact pair terminite is designed to
serve.

This closes a loop most products never get. Building terminite *is* a continuous
test of terminite's thesis. Every friction the owner feels while building it —
every place the human-AI pair is poorly served by today's terminal — is a defect
in the world terminite exists to fix, and therefore an entry on its roadmap. The
roadmap is not a backlog. It is a friction log.

## What "lovely" means

"Lovely" is a hard requirement, not a mood. terminite inherits the bar the best
modern terminals have already set, and treats it as the floor:

- **Instant.** Key-to-screen latency in the single-digit milliseconds. Anything
  a human can perceive as lag is a bug.
- **Belongs.** It honors the platform's conventions — windowing, shortcuts,
  behavior — so that even though terminite draws its own interface, it never
  feels foreign or uncanny.
- **Zero-config.** It is lovely the moment it opens. Configuration is for taste,
  never for repair.
- **Quiet.** It does its job and gets out of the way. Nothing blinks, nags, or
  asks to be admired.

Loveliness is the floor. The thesis is the building above it.

## Principles

A change is checked against these. If it serves none of them, it does not
belong in terminite.

1. **Two users, one surface.** Every feature is judged by how well it serves the
   human *and* the AI at once. Helping one at the other's expense is a failure.
2. **Make the screen legible — both ways.** The human should understand at a
   glance what the AI did; the AI should be able to read the screen as cleanly
   as the human does. Output is structure, not just a stream of bytes.
3. **The friction log is the roadmap.** Features come from real, felt friction
   in the owner's daily work — never from a checklist or a competitor.
4. **One person's taste.** terminite has an owner, not a committee. Openness is
   about sharing the result, not outsourcing the decisions.
5. **Loveliness is non-negotiable.** Speed, native feel, and great defaults are
   the price of entry for every release — not a goal for "later."
6. **Crafted, not skinned.** terminite draws its own interface — GPU-rendered,
   crafted to feel first-class. It is one deliberate surface, never a skinnable
   shell and never a web page in a costume. Not Winamp, and not Electron.
7. **Quiet over clever.** When in doubt, do less, and do it invisibly.

## What is settled, and what is not

The language (**Rust**), the base (**the Zed recipe, built fresh**), the UI
approach (**custom-drawn, GPU-rendered, crafted**), the platform
(**macOS-first**), the modularity (**an out-of-process module protocol**), and
the first feature (**the terminal itself, built modular**) are now decided — see
[decisions.md](decisions.md).

With that, terminite's founding architecture is settled. What stays open, on
purpose, is *which modules get built*. The decisions give terminite a foundation;
they do not give it a feature list. Which capabilities get added, and in what
order, comes from the friction log (see [development.md](development.md)), not
from this document.
