# Collision test — findings

**Date:**
**Agents / vendors / panes:**
**Both confirmed in terminite-auto mode (room_who)?**
**Host log captured (`floor.log`)?**

---

## 1. The claim collision

- Who claimed first (returned **no** conflict):
- Who was refused — and what did the `conflict` field actually say (which holder)?
- Did the refused agent **write anyway**, or correctly **wait**?

## 2. The wake (notify-on-release)

- After the holder released, was the waiter **woken by terminite**, or did it poll?
- Host-side evidence — paste the relevant `[pty-floor]` line(s) (or note the
  channel push) and which pane:
- Latency from `file_release` to the waiter acting:

## 3. The artifact

- Final `target.md` **Entries** — both present, in claim order? (paste them)
- Any clobber / lost entry / out-of-order write:

---

## Verdict

- [ ] **Lock held under a real collision** — first-come-wins, conflict reported, no clobber
- [ ] **Waiter was woken on release** — no poll, host-side evidence
- [ ] **Nothing lost** — both entries survived

**Surprises / new findings:**

**Usage notes (did the namespaced terms / explicit-next-action instruction help?):**
