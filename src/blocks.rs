//! The block Model — the unit Phase 2 is built around.
//!
//! A block is one command + its output: a stable id, the line ranges that
//! cover the command text and its output, an exit code, and timestamps.
//! Blocks are fed by OSC 133 marks (FinalTerm shell integration); the
//! state machine here is deliberately lenient so partial integrations
//! (some shells emit only `A`+`D`) still produce useful blocks.
//!
//! Coordinates: line numbers are *session-absolute row indices*, computed
//! at fire time as `history_size + cursor.line.0`. They stay stable as
//! later output rolls rows into scrollback (the cursor's Line in the live
//! grid would shift, which is why the older "store the cursor Line"
//! convention drifted). The renderer recovers screen vl as
//! `abs - current_history + current_display_offset`.

use std::collections::VecDeque;
use std::time::Instant;

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

/// Per-tab cap. Well past the line count of typical scrollback; oldest
/// evict. Each block is fixed-shape plus a tiny label `Buffer`, so the
/// cap bounds memory at ~hundreds of KB per tab.
pub const MAX_BLOCKS_PER_TAB: usize = 1000;
/// Per-block tag count cap. Beyond this, `add_tag` is a no-op.
pub const MAX_TAGS_PER_BLOCK: usize = 32;
/// Per-tag character cap. Longer tags are rejected.
pub const MAX_TAG_LEN: usize = 64;

/// Matches the existing tab-bar chrome font so a block's `B7` label
/// looks the same as a tab title.
const LABEL_FONT_SIZE: f32 = 18.0;
pub const LABEL_LINE_H: f32 = 26.0;

/// One command + its output, with whatever marks have arrived so far.
/// All `*_line` fields are session-absolute row indices (see module docs).
pub struct Block {
    pub id: u32,
    pub prompt_line: Option<i32>,
    pub command_end_line: Option<i32>,
    pub output_start_line: Option<i32>,
    pub output_end_line: Option<i32>,
    pub exit_code: Option<i32>,
    #[allow(dead_code)]
    pub started_at: Instant,
    #[allow(dead_code)]
    pub finished_at: Option<Instant>,
    /// Pre-shaped `Bn` label, drawn in each pane's left-gutter strip.
    pub label_buffer: Buffer,
    /// Shaped pixel width of `label_buffer`. The renderer right-aligns
    /// the label against the content edge, so it needs to know the label's
    /// actual width to compute the `left` for the TextArea.
    pub label_width: f32,
    /// Free-form labels attached by the pair to give a block a handle
    /// past its id ("flaky-test", "deploy-2026-05-23"). Either side can
    /// add or remove; bounded by `MAX_TAGS_PER_BLOCK` and `MAX_TAG_LEN`.
    pub tags: Vec<String>,
}

impl Block {
    /// The row to anchor the gutter label to. Prefers the **prompt row**
    /// — that's where the user typed the command, the natural "this is
    /// block B7" anchor. Output-anchored labels drift onto the next
    /// block's prompt row when a command has no output (shells fire `D`
    /// after the trailing newline, so `output_end_line` is the row the
    /// cursor advanced to — which the next `A` then claims). Falls back
    /// through the other marks for shells that emit a partial subset.
    pub fn anchor_line(&self) -> Option<i32> {
        self.prompt_line
            .or(self.command_end_line)
            .or(self.output_start_line)
    }
}

/// What a single `on_mark` call transitioned. The proto subscriber
/// needs to fire events on block-open and block-close — `MarkEffect`
/// surfaces both transitions from one call, since an `A` mark can do
/// both (close a previously-open block lossily, then open a fresh one).
#[derive(Default, Debug)]
pub struct MarkEffect {
    pub closed: Option<(u32, Option<i32>)>,
    pub opened: Option<u32>,
}

/// All blocks belonging to one Tab. `open` is the block being built; it
/// graduates to `closed` on the `D` mark (or when the next `A` arrives
/// without a prior `D` — we don't lose blocks just because a shell skips
/// a mark).
pub struct BlockStore {
    closed: VecDeque<Block>,
    open: Option<Block>,
    next_id: u32,
    /// The block this tab's AI cursor is "reading right now," if any.
    /// Set via the proto `cursor_at` method; cleared by `cursor_clear`
    /// or by the cursored block aging out of the closed cap.
    cursor: Option<u32>,
}

impl BlockStore {
    pub fn new() -> Self {
        Self {
            closed: VecDeque::new(),
            open: None,
            next_id: 1,
            cursor: None,
        }
    }

    /// The block currently under the AI cursor, if any.
    pub fn cursor(&self) -> Option<u32> {
        self.cursor
    }

    /// Move the AI cursor to a block. Returns `true` if the block
    /// exists and the cursor moved; `false` if the id is unknown (the
    /// cursor stays where it was).
    pub fn set_cursor(&mut self, block_id: u32) -> bool {
        if self.find(block_id).is_some() {
            self.cursor = Some(block_id);
            true
        } else {
            false
        }
    }

    /// Drop the AI cursor from this tab.
    pub fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    /// Append a tag to a block. Idempotent (re-adding is a no-op), and
    /// bounded by `MAX_TAGS_PER_BLOCK` + `MAX_TAG_LEN`. Returns `true`
    /// on success; `false` if the block is unknown, the tag is empty
    /// or too long, or the cap is hit.
    pub fn add_tag(&mut self, block_id: u32, tag: &str) -> bool {
        let tag = tag.trim();
        if tag.is_empty() || tag.len() > MAX_TAG_LEN {
            return false;
        }
        let Some(block) = self.find_mut(block_id) else { return false };
        if block.tags.iter().any(|t| t == tag) {
            return true; // already present — treat as success
        }
        if block.tags.len() >= MAX_TAGS_PER_BLOCK {
            return false;
        }
        block.tags.push(tag.to_string());
        true
    }

    /// Remove one tag from a block. Returns `true` if the tag was
    /// present; `false` if the block or tag wasn't found.
    pub fn remove_tag(&mut self, block_id: u32, tag: &str) -> bool {
        let Some(block) = self.find_mut(block_id) else { return false };
        if let Some(idx) = block.tags.iter().position(|t| t == tag) {
            block.tags.remove(idx);
            true
        } else {
            false
        }
    }

    fn find_mut(&mut self, id: u32) -> Option<&mut Block> {
        if let Some(open) = self.open.as_mut() {
            if open.id == id {
                return Some(open);
            }
        }
        self.closed.iter_mut().find(|b| b.id == id)
    }

    /// Apply one OSC 133 mark. `line` is the session-absolute row index
    /// at fire time (see module docs). `font_system` is needed so the new
    /// block's label buffer can be shaped immediately — labels render
    /// starting on the very next frame.
    pub fn on_mark(
        &mut self,
        kind: char,
        exit: Option<i32>,
        line: i32,
        font_system: &mut FontSystem,
    ) -> MarkEffect {
        let mut effect = MarkEffect::default();
        match kind {
            'A' => {
                // A prior open block didn't see a 'D' — graduate it
                // lossily rather than losing it.
                if let Some(open) = self.open.take() {
                    let id = open.id;
                    self.push_closed(open);
                    effect.closed = Some((id, None));
                }
                let b = self.fresh_block(Some(line), None, font_system);
                effect.opened = Some(b.id);
                self.open = Some(b);
            }
            'B' => {
                if let Some(b) = self.open.as_mut() {
                    b.command_end_line = Some(line);
                }
            }
            'C' => {
                if let Some(b) = self.open.as_mut() {
                    b.output_start_line = Some(line);
                } else {
                    // Some shells emit `C` without a prior `A`. Open a
                    // block anchored at output-start — its prompt range
                    // stays unknown, which is fine.
                    let b = self.fresh_block(None, Some(line), font_system);
                    effect.opened = Some(b.id);
                    self.open = Some(b);
                }
            }
            'D' => {
                if let Some(mut b) = self.open.take() {
                    b.output_end_line = Some(line);
                    b.exit_code = exit;
                    b.finished_at = Some(Instant::now());
                    effect.closed = Some((b.id, b.exit_code));
                    self.push_closed(b);
                }
                // `D` with no open block: drop. There's nothing to close.
            }
            _ => {} // unknown / future mark letter
        }
        effect
    }

    /// Every block, oldest first, including the currently-open one.
    pub fn iter(&self) -> impl Iterator<Item = &Block> {
        self.closed.iter().chain(self.open.as_ref())
    }

    /// Look up a block by id. Not used by the renderer yet — the module
    /// protocol bundle will consume it.
    #[allow(dead_code)]
    pub fn find(&self, id: u32) -> Option<&Block> {
        self.iter().find(|b| b.id == id)
    }

    fn fresh_block(
        &mut self,
        prompt_line: Option<i32>,
        output_start_line: Option<i32>,
        font_system: &mut FontSystem,
    ) -> Block {
        let id = self.next_id;
        self.next_id += 1;
        let (label_buffer, label_width) = make_label_buffer(font_system, id);
        Block {
            id,
            prompt_line,
            command_end_line: None,
            output_start_line,
            output_end_line: None,
            exit_code: None,
            started_at: Instant::now(),
            finished_at: None,
            label_buffer,
            label_width,
            tags: Vec::new(),
        }
    }

    fn push_closed(&mut self, b: Block) {
        self.closed.push_back(b);
        while self.closed.len() > MAX_BLOCKS_PER_TAB {
            let evicted = self.closed.pop_front();
            // If the AI cursor was pinned to a block that just aged out,
            // clear it — better silent than pointing at nothing.
            if let (Some(evicted), Some(cur)) = (evicted, self.cursor) {
                if evicted.id == cur {
                    self.cursor = None;
                }
            }
        }
    }
}

/// Pre-shape a `Bn` label in the chrome font. Width unconstrained so the
/// shaped text reports its real pixel width — the renderer right-aligns
/// against the content edge using that measurement, so a long label like
/// `B1234` doesn't wrap inside the gutter, it just grows leftward.
fn make_label_buffer(font_system: &mut FontSystem, id: u32) -> (Buffer, f32) {
    let text = format!("B{id}");
    let mut buf = Buffer::new(font_system, Metrics::new(LABEL_FONT_SIZE, LABEL_LINE_H));
    buf.set_size(font_system, None, Some(LABEL_LINE_H));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, &text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    let width = buf.layout_runs().next().map(|r| r.line_w).unwrap_or(0.0);
    (buf, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyphon::FontSystem;

    fn fs() -> FontSystem {
        FontSystem::new()
    }

    #[test]
    fn full_lifecycle_produces_one_closed_block() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        s.on_mark('A', None, 10, &mut fs);
        s.on_mark('B', None, 10, &mut fs);
        s.on_mark('C', None, 11, &mut fs);
        s.on_mark('D', Some(0), 15, &mut fs);
        let v: Vec<_> = s.iter().collect();
        assert_eq!(v.len(), 1);
        let b = v[0];
        assert_eq!(b.id, 1);
        assert_eq!(b.prompt_line, Some(10));
        assert_eq!(b.command_end_line, Some(10));
        assert_eq!(b.output_start_line, Some(11));
        assert_eq!(b.output_end_line, Some(15));
        assert_eq!(b.exit_code, Some(0));
        assert_eq!(b.anchor_line(), Some(10));
    }

    #[test]
    fn a_without_d_graduates_lossily_on_next_a() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        s.on_mark('A', None, 0, &mut fs);
        s.on_mark('A', None, 5, &mut fs);
        let v: Vec<_> = s.iter().collect();
        assert_eq!(v.len(), 2); // first one promoted, second one open
        assert_eq!(v[0].id, 1);
        assert_eq!(v[1].id, 2);
    }

    #[test]
    fn c_without_a_opens_a_block() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        s.on_mark('C', None, 7, &mut fs);
        s.on_mark('D', Some(0), 9, &mut fs);
        let v: Vec<_> = s.iter().collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].prompt_line, None);
        assert_eq!(v[0].output_start_line, Some(7));
        assert_eq!(v[0].anchor_line(), Some(7));
    }

    #[test]
    fn d_without_open_is_a_noop() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        s.on_mark('D', Some(0), 0, &mut fs);
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn old_blocks_evict_at_cap() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        for _ in 0..(MAX_BLOCKS_PER_TAB + 50) {
            s.on_mark('A', None, 0, &mut fs);
            s.on_mark('D', None, 0, &mut fs);
        }
        assert_eq!(s.iter().count(), MAX_BLOCKS_PER_TAB);
    }

    #[test]
    fn anchor_prefers_prompt_then_falls_back() {
        let mut fs = fs();
        // Prompt set → that's the anchor, no matter what else exists.
        let b = BlockStore::new().fresh_block(Some(3), None, &mut fs);
        assert_eq!(b.anchor_line(), Some(3));
        let mut b = BlockStore::new().fresh_block(Some(3), Some(5), &mut fs);
        b.command_end_line = Some(4);
        assert_eq!(b.anchor_line(), Some(3));
        // No prompt (e.g. C-without-A path) → fall back through.
        let b = BlockStore::new().fresh_block(None, Some(5), &mut fs);
        assert_eq!(b.anchor_line(), Some(5));
    }

    fn one_block(s: &mut BlockStore, fs: &mut FontSystem) -> u32 {
        s.on_mark('A', None, 0, fs);
        s.on_mark('D', Some(0), 0, fs);
        s.iter().last().unwrap().id
    }

    #[test]
    fn add_tag_and_list() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        assert!(s.add_tag(id, "flaky"));
        assert!(s.add_tag(id, "tuesday"));
        let block = s.find(id).unwrap();
        assert_eq!(block.tags, vec!["flaky".to_string(), "tuesday".to_string()]);
    }

    #[test]
    fn add_tag_is_idempotent() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        assert!(s.add_tag(id, "x"));
        assert!(s.add_tag(id, "x")); // same tag again — still ok
        assert_eq!(s.find(id).unwrap().tags.len(), 1);
    }

    #[test]
    fn add_tag_rejects_empty_or_too_long() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        assert!(!s.add_tag(id, ""));
        assert!(!s.add_tag(id, "   "));
        let too_long = "x".repeat(MAX_TAG_LEN + 1);
        assert!(!s.add_tag(id, &too_long));
    }

    #[test]
    fn add_tag_caps_per_block() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        for i in 0..MAX_TAGS_PER_BLOCK {
            assert!(s.add_tag(id, &format!("tag-{i}")));
        }
        assert!(!s.add_tag(id, "one-too-many"));
    }

    #[test]
    fn remove_tag_works() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        s.add_tag(id, "a");
        s.add_tag(id, "b");
        assert!(s.remove_tag(id, "a"));
        assert_eq!(s.find(id).unwrap().tags, vec!["b".to_string()]);
        assert!(!s.remove_tag(id, "a")); // already gone
    }

    #[test]
    fn add_tag_on_open_block() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        s.on_mark('A', None, 0, &mut fs);
        let id = s.iter().last().unwrap().id;
        assert!(s.add_tag(id, "in-flight"));
        assert_eq!(s.find(id).unwrap().tags, vec!["in-flight".to_string()]);
    }

    #[test]
    fn set_and_clear_cursor() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        assert!(s.set_cursor(id));
        assert_eq!(s.cursor(), Some(id));
        s.clear_cursor();
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn set_cursor_rejects_unknown_id() {
        let mut s = BlockStore::new();
        assert!(!s.set_cursor(99));
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn cursor_clears_when_pinned_block_evicts() {
        let mut fs = fs();
        let mut s = BlockStore::new();
        let id = one_block(&mut s, &mut fs);
        s.set_cursor(id);
        // Generate enough blocks to push the cursored one off the cap.
        for _ in 0..MAX_BLOCKS_PER_TAB {
            one_block(&mut s, &mut fs);
        }
        assert_eq!(s.cursor(), None);
    }
}
