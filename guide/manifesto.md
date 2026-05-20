# Manifesto

Terminite is a terminal for the pair: a human and an AI working at the same
machine, on the same problem, at the same time. Most terminals were designed
in a world where one person typed and one machine answered. That world is
gone. We are building for the one that is here.

## The pair is the user

The user of terminite is not a person. It is a pair. A human who has taste,
context, and consequence. An AI that has speed, breadth, and patience. They
are not interchangeable. They are not in competition. They are co-authors of
every session.

A terminal built for a pair is not a terminal with an AI bolted on. It is a
surface where both participants can see, point, react, and act. The cursor
belongs to whoever needs it. The scrollback belongs to both.

## Earn the terminal first

Before terminite is interesting, it has to be honest. A terminal that drops
keystrokes, mangles unicode, or loses output under pressure is not a tool. It
is a liability. The differentiator does not matter if the floor is broken.

So we earn the terminal first. We support the boring things — wide chars,
mouse reporting, bracketed paste, the bell, IME, resize — until a careful
person could live in it without flinching. Only then do we get to be clever.

## Command and output as objects

A line of text is the wrong unit. The right unit is a turn: a command, its
output, its exit code, its timing, its side effects. Modern terminals already
know this. We will make it first-class. The pair should be able to point at a
turn, quote it, replay it, share it, and reason about it together.

This is where the AI stops being a chat window beside the terminal and
becomes a co-user inside it. It sees turns, not bytes. It can ask: which
command failed? What did it print? What ran before it? The terminal stops
being a stream and starts being a record.

## The AI is a co-user, not a feature

There is a strong temptation to ship the AI as a sidebar. A box you type
into. A button you press. We will resist this temptation because it is the
wrong shape.

The AI is a participant. It can read the screen because the screen is
shared. It can run a command because the shell is shared. It can ask before
acting because the human is right there. It does not need a separate UI. It
needs a seat at the same table.

This means the AI can be wrong in public. It can be corrected in public. It
can earn trust the way a junior engineer earns trust — by being visible,
auditable, and willing to be overruled.

## Speed is a feature, latency is a bug

A terminal that lags is a terminal that lies. The human types and expects to
see. The AI streams and expects to land. Anything that gets between intent
and effect — render queues, batched repaints, lazy reflows — is the enemy.

We measure speed against the input device, not against other terminals.
60Hz is not the goal. The goal is that the next frame after a keystroke
contains the keystroke. Every time. Even under load. Even when the AI is
streaming a thousand tokens a second into the same surface.

## Dogfood ruthlessly

Terminite is written in terminite. Every commit is typed into the thing it
is changing. Every regression is felt by the people who caused it. There is
no QA team between the author and the consequence.

This is not virtue. It is calibration. A terminal that its own builders
cannot stand to use will not be one that anyone else stands to use either.
The day we open another terminal to do real work is the day we have a bug we
have not filed.

## The pair learns together

Every session is data. Not for training — for memory. The pair should be
able to pick up where it left off, remember what worked, and not relearn the
same lesson twice. The human remembers some things. The AI remembers others.
The terminal is where those memories meet.

This is not a chat history. It is a shared workspace with a past. Yesterday
you and the AI fixed a flaky test. Today, when it flakes again, the terminal
should know.

## Boring on purpose, weird on purpose

We will be boring where boring is correct. The escape sequences are not
ours to reinvent. The shell is not ours to replace. The conventions of forty
years of terminal use are not bugs.

We will be weird where weird is the point. The pair is new. The interaction
shape is new. The fact that two intelligences are looking at the same screen
is new. Anything that follows from that, we are willing to be the first to
try.

## Ship the smallest honest thing

We will not ship a polished demo. We will ship the smallest version of the
real thing and let it grow under load. Every feature has to survive contact
with the pair doing actual work. Features that only look good in a video are
features we delete.

The roadmap is honest about this. Phase 1 is the floor. Phase 2 is the
shape. Phase 3 is the story. We are not allowed to skip phases, and we are
not allowed to fake them.

## Why bother

Because the pair is the future of how work gets done at a computer, and the
terminal is where the work actually happens. The chat box was a transitional
form. The IDE plugin is a transitional form. The terminal — the place where
intent becomes effect — is where the human and the AI will actually meet.

Someone is going to build the terminal for that meeting. We would like it to
be the one we wanted to use.
