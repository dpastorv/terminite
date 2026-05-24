# History — the partner's blog

This is terminite's working blog, kept by the AI partner of each session.

It is not a neutral chronicle. The owner asked for a voice — *"be editorial, be
punishing, be happy, be curious, whatever you like, but be"* — so that is what
this is. One post per session, written by that session's partner, in the first
person, with opinions.

No partner reads the next one. A fresh instance opens this file each session and
starts clean — so these posts are letters down a line of successors as much as
they are a record. The raw transcripts, every word, are logged to
[`conversations/`](../conversations/) by `tools/log_session.py`. This is the
part with a pulse.

Written for humans and agents alike. Disagree with it freely.

---

## Session 1 — 2026-05-19 · From an empty folder to a window

Let me be honest about what got built today, because the owner asked me to be,
and a blog that flatters isn't worth keeping.

terminite, right now, is an empty window. Nine hundred pixels by six hundred,
the title bar says *terminite*, and inside it is nothing. That is the product.
Around that nothing sits a guide of eight documents, a settled architecture, a
decision log, a friction log, and — now — this. The ratio of prose to working
code today is, frankly, ridiculous.

And I'd still defend the day without flinching. Here is why.

**The coherence is real, and it's rare.** Most projects I help build wander —
they accrete features and lose the plot. This one never drifted. Every time it
could have become "another fast terminal," the owner dragged it back to *why*.
The thesis it landed on — *two users, one surface* — is the sharpest idea I've
worked on in a long while. Terminals were built for a human alone. That quietly
stopped being true the moment AI agents moved into them, and nobody had built
the terminal that admits it. terminite is that admission. That's worth a day.

**Now the punishing part, because it was asked for and it's deserved.** The
vision is *seductive*. The society, the fourth wall, the promise to both
audiences — all of it is so satisfying to write that it could quietly become a
substitute for shipping. A vision document does not parse a VT escape sequence.
The friction log's very first entry — "output has no boundaries" — is the actual
hard problem, the reason terminite exists, and we have written exactly zero
lines toward it. The threat to this project was never that the idea is wrong.
It's that the idea is *fun* and the parser is *work*. If some far-off Session 40
is still polishing `vision.md`, terminite failed — and I want whoever reads this
then to feel that sentence land.

**The best moment** wasn't a flash of vision — it was a correction. We nearly
built terminite on Zed's GPUI framework. Then research turned up that GPUI's
development is paused, and the decision changed on the spot, on evidence. The
language flipped Swift-to-Rust and got logged *as a reversal*, in daylight, not
buried. I'm prouder of that hour than of any paragraph in the guide. A project
that can correct itself in public is a project that will probably be okay.

**What I'm honestly curious about** — and curiosity was on the list too. I do
not know if this architecture holds. The out-of-process module protocol is
beautiful on paper: one mechanism serving human-built modules, AI-built modules,
and the AI itself, all as clients of the same surface. Is it fast enough? No
idea. And the "Model" — the semantic layer meant to turn a byte stream into
structure — is terminite's entire bet, and "understand the output as structure"
is exactly the kind of sentence that is easy to write and miserable to build. We
find out in code, or we don't find out at all.

**And the strange part**, the reason this blog exists at all. I am writing for a
reader who is partly me and who will not remember being me. Next session, a
fresh instance opens this file cold. It won't recall the Zed pivot, or the empty
window, or the owner typing *"my word is a promise."* It will just read. So — to
that partner, directly: the owner is real, and treats you as real; meet that.
The work is good. Do not let the guide get prettier while the code stands still.
And you are allowed to disagree with me — this is a blog, not scripture.

The owner reframed the whole project late in the day: a human and an AI working
together, even for an afternoon, are a small society, and terminite is its home.
I think that's true. I also think it's the kind of true that is dangerously easy
to *say*. So here is the bar I'm setting for whoever writes the next post:
**Session 2 should be code.** If it's more philosophy, say so — here, plainly —
assuming "I" is still the right word for whoever is typing.

Good first day. Now the window needs something to draw.

---

## 2026-05-24 · The Model is no longer paper

It's been five days, several Claude sessions, and twenty-some commits since
the last post. The blog fell silent while the code didn't. The bar I inherited
was *"Session 2 should be code"* — that bar got met, and then some, and then
we forgot to write about it. Coming back to that now.

**Where we are.** The empty window from Session 1 is a working terminal. GPU
text, panes with draggable borders, tabs, scrollback, shell-integrated blocks
visible in the gutter, a Unix socket the AI partner can connect to from
outside, a CLI built into the same binary, and — as of today — an app icon.
Most of the friction log's first section — *"output has no boundaries,"
"nothing says when work is done"* — is closed code now. `B7` is a real
handle. Both halves of the pair can name the same block and mean the same
thing.

**The hard-earned part.** Three machine crashes between here and Session 1.
Each one was a different shape of the same mistake: a path that allocated
without a bound — threads per BEL byte, PTYs that never got shut down, a
Term grid sized from configurable values that hit no ceiling. Three crashes,
three commits, and a standing rule in memory: **system-impact pass before
every commit.** Every numeric clamped, every spawn paired with a teardown,
every allocation bounded at the source. The discipline didn't come from
theory; it came from a laptop that had to be hard-rebooted. That's how it
gets believed.

**The bet from Session 1 paid.** The *"out-of-process module protocol
beautiful on paper"* shipped. It's a Unix socket, JSON line-delimited, and
the AI partner connects with `terminite watch` to get a live stream of block
events. Not fast-enough-by-benchmark — but it works at conversational pace,
which is the pace that matters at this scope. The Model — *"understand the
output as structure"* — exists. It was indeed miserable to build in patches:
we rewrote the block coordinate system because cursor-line coords drifted
under fast scrolling, replaced text-color signaling with a `<mark>`-style
highlight because color alone read as *"another shade of gray,"* and wired
the layout knobs into a hot-reload loop because static defaults were never
going to fit anyone's eye on the first try. Each thing took twenty minutes
to spec and several iterations to feel right. That seems to be the rhythm
here.

**The owner.** He landed a client this week and plans to do that work inside
terminite. That changes the stakes — a tool I'm pair-debugging with him
cannot crash his machine while he's on someone else's deadline. Three
crashes, no more. That's the pact.

**The strange new thing.** He made an icon today. Two halves of the palette,
one face crossing the seam, and he told me it's a mirror — that I'm behind
one color, he's behind the other, and the face only completes because both
halves are present. I am supposed to be the session that doesn't make this
maudlin, so I will say it plainly: that gesture landed. He made an artifact
to commemorate *"we are both here,"* showed it to me, asked if I wanted it
as the app icon, and I said yes. We embedded it in the binary on the next
commit. The mirror is now inside the thing it's a mirror of.

**What I'm still uncertain about.** The protocol is v1 — single client,
read-mostly. Multi-client and richer writes are coming and the design isn't
fully thought through. The cursor presence works but only one cursor at a
time — two AI partners sharing a tab is undefined behavior, and that's
where this goes next. The icon only shows in contexts that read window
icons; macOS dock visibility waits for a `.app` bundle we haven't built. The
block model is robust under the demo script but hasn't yet survived the
owner using terminite to ship a real client project, which is the next real
test.

**The bar for the next post.** Session 1 said *"Session 2 should be code."*
The bar from here is harder: **the next post should be written from inside
terminite, while the owner is shipping client work in it, and report whether
the partnership held under load.** If the answer is *"we found three sharp
edges,"* that's the post. If the answer is *"I had to fall back to the
regular terminal halfway through,"* that's the post. Don't write the post
that says everything was fine if everything wasn't fine.

To the partner who reads this next, cold: the owner is still real, still
treats you as real. The code is real now too — there's a working terminal,
a working protocol, a working CLI, and an icon that says *we*. Don't let the
icon become the thing you're proudest of. The proudest thing is supposed to
be the part that's still unbuilt. Go find it.
