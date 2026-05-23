//! The block Model — the unit Phase 2 is built around.
//!
//! A block is one command + its output: a stable id, the line ranges that
//! cover the command text and its output, an exit code, and timestamps.
//! Blocks are fed by OSC 133 marks (FinalTerm shell integration); the
//! state machine here is deliberately lenient so partial integrations
//! (some shells emit only `A`+`D`) still produce useful blocks.
//!
//! Coordinates: line numbers are stored in the same absolute coordinate
//! system selections use (`viewport_line - display_offset_at_fire`), so a
//! block's anchor row is reproducible by `abs + current_display_offset`
//! as the user scrolls.

use std::collections::VecDeque;
use std::time::Instant;

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

/// Per-tab cap. Well past the line count of typical scrollback; oldest
/// evict. Each block is fixed-shape plus a tiny label `Buffer`, so the
/// cap bounds memory at ~hundreds of KB per tab.
pub const MAX_BLOCKS_PER_TAB: usize = 1000;

/// Matches the existing tab-bar chrome font so a block's `B7` label
/// looks the same as a tab title.
const LABEL_FONT_SIZE: f32 = 18.0;
const LABEL_LINE_H: f32 = 26.0;
const LABEL_BUFFER_WIDTH: f32 = 60.0;

/// One command + its output, with whatever marks have arrived so far.
/// All line numbers are in terminite's absolute-line convention.
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
}

impl Block {
    /// The row to anchor the gutter label to: output-start if known, else
    /// the command-end row, else the prompt-start row. `None` is unusual
    /// (means we have a Block with no positional information at all).
    pub fn anchor_line(&self) -> Option<i32> {
        self.output_start_line
            .or(self.command_end_line)
            .or(self.prompt_line)
    }
}

/// All blocks belonging to one Tab. `open` is the block being built; it
/// graduates to `closed` on the `D` mark (or when the next `A` arrives
/// without a prior `D` — we don't lose blocks just because a shell skips
/// a mark).
pub struct BlockStore {
    closed: VecDeque<Block>,
    open: Option<Block>,
    next_id: u32,
}

impl BlockStore {
    pub fn new() -> Self {
        Self { closed: VecDeque::new(), open: None, next_id: 1 }
    }

    /// Apply one OSC 133 mark. `line` is the cursor's absolute line at
    /// fire time. `font_system` is needed so the new block's label buffer
    /// can be shaped immediately — labels render starting on the very
    /// next frame.
    pub fn on_mark(
        &mut self,
        kind: char,
        exit: Option<i32>,
        line: i32,
        font_system: &mut FontSystem,
    ) {
        match kind {
            'A' => {
                // A prior open block didn't see a 'D' — graduate it
                // lossily rather than losing it.
                if let Some(open) = self.open.take() {
                    self.push_closed(open);
                }
                self.open = Some(self.fresh_block(Some(line), None, font_system));
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
                    self.open = Some(self.fresh_block(None, Some(line), font_system));
                }
            }
            'D' => {
                if let Some(mut b) = self.open.take() {
                    b.output_end_line = Some(line);
                    b.exit_code = exit;
                    b.finished_at = Some(Instant::now());
                    self.push_closed(b);
                }
                // `D` with no open block: drop. There's nothing to close.
            }
            _ => {} // unknown / future mark letter
        }
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
        Block {
            id,
            prompt_line,
            command_end_line: None,
            output_start_line,
            output_end_line: None,
            exit_code: None,
            started_at: Instant::now(),
            finished_at: None,
            label_buffer: make_label_buffer(font_system, id),
        }
    }

    fn push_closed(&mut self, b: Block) {
        self.closed.push_back(b);
        while self.closed.len() > MAX_BLOCKS_PER_TAB {
            self.closed.pop_front();
        }
    }
}

/// Pre-shape a `Bn` label in the chrome font. Sized to a fixed small
/// width — anything past it is clipped by the gutter's `TextBounds`.
fn make_label_buffer(font_system: &mut FontSystem, id: u32) -> Buffer {
    let text = format!("B{id}");
    let mut buf = Buffer::new(font_system, Metrics::new(LABEL_FONT_SIZE, LABEL_LINE_H));
    buf.set_size(font_system, Some(LABEL_BUFFER_WIDTH), Some(LABEL_LINE_H));
    let attrs = Attrs::new().family(Family::Monospace);
    buf.set_text(font_system, &text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(font_system, false);
    buf
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
        assert_eq!(b.anchor_line(), Some(11));
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
    fn anchor_falls_back_through_marks() {
        let mut fs = fs();
        let mut b = BlockStore::new().fresh_block(Some(3), None, &mut fs);
        assert_eq!(b.anchor_line(), Some(3));
        b.command_end_line = Some(4);
        assert_eq!(b.anchor_line(), Some(4));
        b.output_start_line = Some(5);
        assert_eq!(b.anchor_line(), Some(5));
    }
}
