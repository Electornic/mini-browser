// Effective overflow clip per box (compat round 3 step 3, docs/compat-round3.md
// "3단계 — overflow 클리핑").
//
// One table, computed once after layout, that paint, hit testing, the agent's
// click target and the corpus probe all read — so the four can't disagree
// about what is clipped. Keyed by `NodeId`: an anonymous box takes the clip
// of its nearest node ancestor (the caller knows it from its walk).
//
// Probe commit (step 3 P): the table is always empty — the engine doesn't
// clip yet, and the probe's "before" numbers measure exactly that. Step 3a
// fills `compute_clips` (the same way step 2.1's probe commit shipped a
// `media_verdict` that the engine commit then changed).

use std::collections::HashMap;

use crate::dom::NodeId;

use super::{LayoutBox, Rect};

/// Each clipped box's effective clip rectangle, in page (layout) coordinates,
/// already intersected with every clipping ancestor's. A node that isn't in
/// the table paints unclipped. A zero-area entry (`clip: rect(0 0 0 0)`,
/// `width: 1px; height: 0; overflow: hidden`) hides its whole box.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipTable {
    clips: HashMap<NodeId, Rect>,
}

impl ClipTable {
    /// The clip the boxes of `node` paint under; `None` = unclipped.
    pub fn clip_of(&self, node: NodeId) -> Option<Rect> {
        self.clips.get(&node).copied()
    }

    /// What is left of `rect` (a box of `node`) after its clip: `rect` itself
    /// when unclipped, `None` when nothing of it is visible.
    pub fn visible_rect(&self, node: NodeId, rect: Rect) -> Option<Rect> {
        match self.clip_of(node) {
            None => Some(rect),
            Some(clip) => intersect(rect, clip),
        }
    }

    /// Number of clipped nodes.
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
}

/// The effective clip of every box under `root`.
///
/// Step 3 P: nothing clips yet, so the table is empty (see the module
/// comment); step 3a implements the walk.
pub fn compute_clips(_root: &LayoutBox) -> ClipTable {
    ClipTable::default()
}

/// `a ∩ b`, or `None` when they share no area (touching edges included).
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn intersect_overlapping_disjoint_and_touching_rects() {
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 10.0, 10.0)),
            Some(rect(5.0, 5.0, 5.0, 5.0))
        );
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(20.0, 0.0, 5.0, 5.0)),
            None
        );
        // Sharing an edge is no area.
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 0.0, 5.0, 5.0)),
            None
        );
        // A zero-area clip hides everything.
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(2.0, 2.0, 0.0, 0.0)),
            None
        );
    }
}
