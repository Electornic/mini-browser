// Can a click reach an element the way it would in Chrome? (compat round 4
// step 0, P2 — docs/compat-round4.md.)
//
// Two halves:
//   * The geometry the agent's click uses — where an element can be
//     clicked (`visible_rect`), where `click` scrolls to bring it into view
//     (`scroll_target` + the engine's own `clamped_scroll`), and the window
//     point it presses (`click_point`). `AgentBrowser::click` calls exactly
//     these, so the measurement and the action can't drift apart.
//   * `classify`: the frozen `obs_unreachable` categories for one observed
//     element — `hidden`, `clipped`, `offscreen_x`, `intercepted` (with its
//     cause), first match wins — plus the frozen ours/browser-too split
//     (P2b). The probe counts these per site; nothing in the browser's
//     behaviour reads them.
//
// The classification measures Chrome's rules from the start, not ours:
// links are judged by the hit test like any other element (in Chrome a
// covered link doesn't navigate; here it still does until step 1b), and the
// pressed point is the in-view centre (step 1c), not today's box centre.

use std::collections::BTreeMap;

use mb_runtime::chrome::CHROME_HEIGHT;
use mb_runtime::css::{Unit, Value};
use mb_runtime::dom::{Document, NodeId};
use mb_runtime::input::WindowInput;
use mb_runtime::layout::{BoxType, ClipTable, LayoutBox, Rect};
use mb_runtime::render;
use mb_runtime::state::clamped_scroll;
use mb_runtime::style::StyledNode;
use mb_runtime::view;

use crate::index;

/// The page viewport in CSS px (the browser window minus the chrome).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

/// Where `id` can be clicked, in page coordinates: its boxes under their
/// transforms, cut to their overflow clips, unioned. `None` = no box;
/// `Some(Err(()))` = boxes exist but the clip hides all of them.
pub fn visible_rect(
    layout_root: &LayoutBox,
    id: NodeId,
    clips: &ClipTable,
) -> Option<Result<Rect, ()>> {
    index::find_visible_rect(layout_root, id, clips)
}

/// The scroll offset `click` asks for to bring `rect` into view — its
/// centre at the viewport's centre — or `None` when the box already fits
/// in the viewport at `scroll`. The engine then clamps it
/// ([`clamped_scroll`]).
pub fn scroll_target(rect: Rect, scroll: f32, viewport_height: f32) -> Option<f32> {
    if rect.y >= scroll && rect.y + rect.height <= scroll + viewport_height {
        return None;
    }
    Some((rect.y + rect.height / 2.0 - viewport_height / 2.0).max(0.0))
}

/// The scroll offset the page is at when `click` presses `rect`: the
/// wheel scroll of [`scroll_target`], clamped by the engine's own rule.
pub fn scroll_for_click(rect: Rect, scroll: f32, viewport: Viewport, document_height: f32) -> f32 {
    match scroll_target(rect, scroll, viewport.height) {
        Some(target) => clamped_scroll(target, window_height(viewport), document_height),
        None => scroll,
    }
}

/// Today's click point (window coordinates, chrome included): the box
/// centre, with y pulled into the visible part of a box taller than the
/// viewport. x is the box centre even when that is outside the window.
pub fn click_point(rect: Rect, scroll: f32, viewport: Viewport) -> (f32, f32) {
    let top = rect.y.max(scroll);
    let bottom = (rect.y + rect.height).min(scroll + viewport.height);
    let doc_y = if bottom > top {
        (top + bottom) / 2.0
    } else {
        rect.y + rect.height / 2.0
    };
    (rect.x + rect.width / 2.0, doc_y - scroll + CHROME_HEIGHT)
}

/// WebDriver's in-view centre point (window coordinates): the centre of
/// `rect ∩ viewport` on both axes. `None` when the horizontal intersection
/// is empty. y falls back like [`click_point`] (vertical scrolling brings
/// the box in; there is no horizontal scrolling).
pub fn in_view_centre(rect: Rect, scroll: f32, viewport: Viewport) -> Option<(f32, f32)> {
    let left = rect.x.max(0.0);
    let right = (rect.x + rect.width).min(viewport.width);
    if right <= left {
        return None;
    }
    let (_, y) = click_point(rect, scroll, viewport);
    Some(((left + right) / 2.0, y))
}

/// Why the page offers `id` to nobody at all (compat round 4 step 1a):
/// nothing of it shows, or it sits in an `inert` subtree. The agent's
/// click and `type` refuse these before pressing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concealment {
    /// Computed `visibility: hidden`/`collapse` (D5).
    Hidden,
    /// Inside an `inert` subtree (N4).
    Inert,
}

/// [`Concealment`] of `id` from its (first) layout box's computed style;
/// `None` when it shows or has no box.
pub fn concealment(layout_box: &LayoutBox, id: NodeId) -> Option<Concealment> {
    if let Some(node) = styled(layout_box)
        && node.node_id == id
    {
        return if node.visibility_hidden() {
            Some(Concealment::Hidden)
        } else if node.inert() {
            Some(Concealment::Inert)
        } else {
            None
        };
    }
    layout_box
        .children
        .iter()
        .find_map(|child| concealment(child, id))
}

fn window_height(viewport: Viewport) -> usize {
    (viewport.height + CHROME_HEIGHT).max(0.0) as usize
}

/// Why a click doesn't land where an agent (or a person) means it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The click lands on the element (or its label).
    Reached,
    /// No box, or a zero-size one (the click refuses; not in the union).
    NoBox,
    /// `visibility: hidden/collapse` (nearest declaration up the tree), or
    /// `inert` on it or an ancestor.
    Hidden,
    /// Its overflow clip hides all of it (the click refuses).
    Clipped,
    /// Nothing of its visible box is inside the viewport's width.
    OffscreenX { browser_too: bool },
    /// Something else is hit at the in-view centre.
    Intercepted(Interception),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interception {
    pub cause: InterceptCause,
    /// What the hit test picked.
    pub by: Option<NodeId>,
    /// P2b browser-too by a cover: a fixed / sticky / absolute box over
    /// at least 30% of the viewport (the first frozen table).
    pub overlay: bool,
    /// P2b browser-too by a vertical author offset: the visible box lies
    /// wholly above the page top (added after `r4-p0`, user decision
    /// 2026-11-28).
    pub above_page: bool,
}

impl Interception {
    /// Chrome's click fails the same way (either browser-too row).
    pub fn browser_too(self) -> bool {
        self.overlay || self.above_page
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptCause {
    /// The hit box has `pointer-events: none` (nearest declaration):
    /// Chrome's click would go through it.
    PointerNoneOverlay,
    /// Hit-tested in paint order (z-index), the element is what's on top.
    ZOrder,
    Other,
}

impl Reach {
    /// The category name (`unreachable.txt`), `None` when reachable or not
    /// in the union.
    pub fn category(self) -> Option<&'static str> {
        match self {
            Self::Reached | Self::NoBox => None,
            Self::Hidden => Some("hidden"),
            Self::Clipped => Some("clipped"),
            Self::OffscreenX { .. } => Some("offscreen_x"),
            Self::Intercepted(_) => Some("intercepted"),
        }
    }

    /// The P2b split: does Chrome fail the same way? (`hidden`/`clipped`
    /// are always ours — Chrome doesn't offer them as targets at all.)
    pub fn browser_too(self) -> bool {
        match self {
            Self::OffscreenX { browser_too } => browser_too,
            Self::Intercepted(i) => i.browser_too(),
            _ => false,
        }
    }
}

/// What one layout walk learns about each element's box, so classifying
/// many elements doesn't rewalk the tree for styles.
#[derive(Debug, Clone, Copy, Default)]
struct BoxFacts {
    /// Computed `visibility` is hidden / collapse. (Step 0 read the
    /// nearest declaration up the tree — the engine didn't inherit
    /// `visibility` then; the inherited computed value is the same answer,
    /// P1 → step 1a.)
    hidden: bool,
    /// Computed `pointer-events: none` (inherited since step 1b — the
    /// nearest declaration step 0 read gives the same answer).
    pointer_none: bool,
    /// `position` is fixed / sticky / absolute.
    overlay_position: bool,
    /// It or an ancestor is moved off the left by its author: a negative
    /// `translateX`, or `left`/`right`/`margin-left` at least a viewport
    /// width negative (P2b browser-too `offscreen_x`).
    author_offset: bool,
}

/// The page one classification runs against.
pub struct ReachContext<'a> {
    document: &'a Document,
    layout_root: &'a LayoutBox,
    clips: &'a ClipTable,
    document_height: f32,
    viewport: Viewport,
    /// The page's scroll offset before the click scrolls.
    scroll: f32,
    facts: BTreeMap<NodeId, BoxFacts>,
}

impl<'a> ReachContext<'a> {
    pub fn new(
        document: &'a Document,
        layout_root: &'a LayoutBox,
        clips: &'a ClipTable,
        document_height: f32,
        viewport: Viewport,
        scroll: f32,
    ) -> Self {
        let mut facts = BTreeMap::new();
        collect_facts(layout_root, BoxFacts::default(), viewport.width, &mut facts);
        Self {
            document,
            layout_root,
            clips,
            document_height,
            viewport,
            scroll,
            facts,
        }
    }

    /// `visibility` hidden by its nearest declaration (P1's
    /// `obs_hidden_visibility`).
    pub fn visibility_hidden(&self, id: NodeId) -> bool {
        self.facts.get(&id).is_some_and(|f| f.hidden)
    }

    /// `pointer-events: none` by its nearest declaration (`obs_pointer_none`).
    pub fn pointer_events_none(&self, id: NodeId) -> bool {
        self.facts.get(&id).is_some_and(|f| f.pointer_none)
    }

    /// `inert` on the element or an ancestor.
    pub fn inert(&self, id: NodeId) -> bool {
        self.ancestors_or_self(id).any(|n| {
            self.document
                .element_data(n)
                .is_some_and(|e| e.attributes.contains_key("inert"))
        })
    }

    /// Where a click on `id` happens: the page's scroll offset after
    /// `click` scrolls it into view (clamped like the engine does), and the
    /// in-view centre it would press. `None` without a visible box or with
    /// no horizontal overlap with the viewport.
    pub fn click_position(&self, id: NodeId) -> Option<(f32, (f32, f32))> {
        let visible = visible_rect(self.layout_root, id, self.clips)?.ok()?;
        let scroll = scroll_for_click(visible, self.scroll, self.viewport, self.document_height);
        Some((scroll, in_view_centre(visible, scroll, self.viewport)?))
    }

    /// The frozen `obs_unreachable` category of element `id`.
    pub fn classify(&self, id: NodeId) -> Reach {
        if self.visibility_hidden(id) || self.inert(id) {
            return Reach::Hidden;
        }
        if !index::find_rect(self.layout_root, id).is_some_and(|r| r.width > 0.0 && r.height > 0.0)
        {
            return Reach::NoBox;
        }
        let visible = match visible_rect(self.layout_root, id, self.clips) {
            Some(Ok(visible)) => visible,
            Some(Err(())) => return Reach::Clipped,
            None => return Reach::NoBox,
        };
        let scroll = scroll_for_click(visible, self.scroll, self.viewport, self.document_height);
        let Some(point) = in_view_centre(visible, scroll, self.viewport) else {
            let browser_too = self
                .ancestors_or_self(id)
                .any(|n| self.facts.get(&n).is_some_and(|f| f.author_offset));
            return Reach::OffscreenX { browser_too };
        };
        let input = WindowInput {
            mouse_position: Some(point),
            ..WindowInput::default()
        };
        let hit = view::compute_hovered_hit(&input, self.layout_root, self.clips, scroll)
            .and_then(|hit| hit.node_id);
        if hit.is_some_and(|hit| self.reaches(hit, id)) {
            return Reach::Reached;
        }
        let cause = if hit.is_some_and(|hit| self.pointer_events_none(hit)) {
            InterceptCause::PointerNoneOverlay
        } else if view::hit_node_in_paint_order(&input, self.layout_root, self.clips, scroll)
            .is_some_and(|painted| self.reaches(painted, id))
        {
            InterceptCause::ZOrder
        } else {
            InterceptCause::Other
        };
        let overlay = cause != InterceptCause::ZOrder
            && hit.is_some_and(|hit| self.covering_overlay(hit, id, scroll));
        Reach::Intercepted(Interception {
            cause,
            by: hit,
            overlay,
            above_page: visible.y + visible.height <= 0.0,
        })
    }

    fn reaches(&self, hit: NodeId, target: NodeId) -> bool {
        hit_reaches(self.document, hit, target)
    }

    /// P2b browser-too `intercepted`: between the hit and the element they
    /// share, a fixed / sticky / absolute box covers at least 30% of the
    /// viewport (cookie banners, modal backdrops — Chrome is covered too).
    fn covering_overlay(&self, hit: NodeId, target: NodeId, scroll: f32) -> bool {
        let target_line: Vec<NodeId> = self.ancestors_or_self(target).collect();
        let viewport = Rect {
            x: 0.0,
            y: scroll,
            width: self.viewport.width,
            height: self.viewport.height,
        };
        let floor = 0.3 * self.viewport.width * self.viewport.height;
        self.ancestors_or_self(hit)
            .take_while(|n| !target_line.contains(n))
            .filter(|n| self.facts.get(n).is_some_and(|f| f.overlay_position))
            .any(|n| {
                visible_rect(self.layout_root, n, self.clips)
                    .and_then(Result::ok)
                    .is_some_and(|r| intersection_area(r, viewport) >= floor)
            })
    }

    fn ancestors_or_self(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), |&n| self.document.get(n)?.parent)
    }
}

/// A click whose hit is `hit` acts on `target`: `hit` is the target or
/// inside it, or inside a `<label>` that labels it (label activation starts
/// from the hit node too). The agent's click refuses otherwise (step 1b).
pub fn hit_reaches(document: &Document, hit: NodeId, target: NodeId) -> bool {
    ancestors_or_self(document, hit).any(|n| {
        n == target
            || (document
                .element_data(n)
                .is_some_and(|e| e.tag_name == "label")
                && label_labels(document, n, target))
    })
}

/// `label` labels `target`: by `for` = its `id`, or (no `for`) by holding it.
pub fn label_labels(document: &Document, label: NodeId, target: NodeId) -> bool {
    match document
        .element_data(label)
        .and_then(|e| e.attributes.get("for"))
    {
        Some(for_id) => document
            .element_data(target)
            .and_then(|e| e.attributes.get("id"))
            .is_some_and(|id| id == for_id),
        None => ancestors_or_self(document, target).any(|n| n == label),
    }
}

fn ancestors_or_self(document: &Document, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(Some(id), |&n| document.get(n)?.parent)
}

fn intersection_area(a: Rect, b: Rect) -> f32 {
    let w = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let h = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    w.max(0.0) * h.max(0.0)
}

fn collect_facts(
    layout_box: &LayoutBox,
    inherited: BoxFacts,
    viewport_width: f32,
    out: &mut BTreeMap<NodeId, BoxFacts>,
) {
    let facts = match styled(layout_box) {
        Some(node) => {
            // Text boxes too: they are what most clicks hit.
            let facts = own_facts(layout_box, node, inherited, viewport_width);
            out.entry(node.node_id).or_insert(facts);
            facts
        }
        None => inherited,
    };
    for child in &layout_box.children {
        collect_facts(child, facts, viewport_width, out);
    }
}

fn own_facts(
    layout_box: &LayoutBox,
    node: &StyledNode,
    inherited: BoxFacts,
    viewport_width: f32,
) -> BoxFacts {
    let keyword = |name: &str| match node.value(name) {
        Some(Value::Keyword(k)) => Some(k.as_str()),
        _ => None,
    };
    let hidden = node.visibility_hidden();
    let pointer_none = node.pointer_events_none();
    let far_left = |name: &str| matches!(node.value(name), Some(Value::Length(v, Unit::Px)) if *v <= -viewport_width);
    let author_offset = inherited.author_offset
        || render::transform_for(layout_box).e < 0.0
        || far_left("left")
        || far_left("right")
        || far_left("margin-left");
    BoxFacts {
        hidden,
        pointer_none,
        overlay_position: matches!(keyword("position"), Some("fixed" | "sticky" | "absolute")),
        author_offset,
    }
}

fn styled(layout_box: &LayoutBox) -> Option<&StyledNode> {
    match &layout_box.box_type {
        BoxType::BlockNode(node)
        | BoxType::FlexNode(node)
        | BoxType::GridNode(node)
        | BoxType::TableNode(node) => Some(node),
        BoxType::AnonymousBlock => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mb_runtime::view::{DocumentView, build_document_view};
    use mb_runtime::{css, html, style};

    use super::*;

    const VIEWPORT: Viewport = Viewport {
        width: 400.0,
        height: 300.0,
    };

    struct Page {
        document: Document,
        view: DocumentView,
    }

    impl Page {
        fn new(html_source: &str, css_source: &str) -> Self {
            let document = html::parse(html_source).unwrap();
            let sheet = css::parse(css_source).unwrap();
            let view = build_document_view(
                &document,
                std::slice::from_ref(&sheet),
                VIEWPORT.width as usize,
                VIEWPORT.height as usize,
                None,
                &HashMap::new(),
                style::InteractionState::default(),
            )
            .unwrap();
            Self { document, view }
        }

        fn context(&self) -> ReachContext<'_> {
            ReachContext::new(
                &self.document,
                &self.view.layout_root,
                &self.view.clips,
                self.view.document_height,
                VIEWPORT,
                0.0,
            )
        }

        fn by_id(&self, id_attr: &str) -> NodeId {
            let mut stack = self.document.roots().to_vec();
            while let Some(n) = stack.pop() {
                if self
                    .document
                    .element_data(n)
                    .and_then(|e| e.attributes.get("id"))
                    .is_some_and(|v| v == id_attr)
                {
                    return n;
                }
                stack.extend(
                    self.document
                        .get(n)
                        .map(|d| d.children.clone())
                        .unwrap_or_default(),
                );
            }
            panic!("no #{id_attr}")
        }

        fn reach(&self, id_attr: &str) -> Reach {
            self.context().classify(self.by_id(id_attr))
        }
    }

    const OVERLAY: &str =
        "#cover { position: fixed; left: 0; top: 0; width: 400px; height: 300px; }";

    #[test]
    fn hidden_wins_over_clipped() {
        // The menu is both `visibility: hidden` (on its ancestor — the old
        // engine doesn't inherit it, so the nearest declaration counts) and
        // clipped away by a zero-height overflow box.
        let page = Page::new(
            "<html><body><div id=menu><div id=box><a id=item href=/x>Item</a></div></div><a id=shown href=/y>Shown</a></body></html>",
            "#menu { visibility: hidden; } #box { height: 0; overflow: hidden; }",
        );
        assert_eq!(page.reach("item"), Reach::Hidden);
        assert_eq!(page.reach("shown"), Reach::Reached);
        // A `visible` closer to the element turns it back on.
        let page = Page::new(
            "<html><body><div id=menu><a id=item href=/x>Item</a></div></body></html>",
            "#menu { visibility: hidden; } #item { visibility: visible; }",
        );
        assert_eq!(page.reach("item"), Reach::Reached);
        // Clipped alone stays clipped.
        let page = Page::new(
            "<html><body><div id=box><a id=item href=/x>Item</a></div></body></html>",
            "#box { height: 0; overflow: hidden; }",
        );
        assert_eq!(page.reach("item"), Reach::Clipped);
    }

    #[test]
    fn inert_subtree_is_hidden() {
        let page = Page::new(
            "<html><body><div inert><button id=b>Go</button></div></body></html>",
            "",
        );
        assert_eq!(page.reach("b"), Reach::Hidden);
    }

    #[test]
    fn offscreen_x_is_an_empty_horizontal_intersection_not_an_off_screen_centre() {
        // A 5,117px-wide box: its centre is far right of the window, but
        // part of it is on screen — reachable at the in-view centre.
        let page = Page::new(
            "<html><body><div id=wide>Wide</div><div id=right>Right</div><a id=skip href=#main>Skip</a></body></html>",
            "body { margin: 0; } #wide { width: 5117px; height: 20px; } \
             #right { margin-left: 600px; width: 100px; height: 20px; } \
             #skip { position: absolute; left: -9999px; top: 0; }",
        );
        assert_eq!(page.reach("wide"), Reach::Reached);
        // Pushed past the right edge by layout: ours.
        assert_eq!(
            page.reach("right"),
            Reach::OffscreenX { browser_too: false }
        );
        // A skip link parked at -9999px: Chrome can't show it either.
        assert_eq!(page.reach("skip"), Reach::OffscreenX { browser_too: true });
        let wide = page.by_id("wide");
        let rect = visible_rect(&page.view.layout_root, wide, &page.view.clips)
            .unwrap()
            .unwrap();
        assert_eq!(
            in_view_centre(rect, 0.0, VIEWPORT).map(|p| p.0),
            Some(200.0)
        );
        assert!(
            click_point(rect, 0.0, VIEWPORT).0 > 2000.0,
            "today: box centre"
        );
    }

    #[test]
    fn link_under_an_overlay_is_intercepted_like_chrome() {
        // Today the link still navigates (geometric LinkTarget match), but
        // in Chrome the click lands on the cover: measured by Chrome's rule.
        let page = Page::new(
            "<html><body><a id=link href=/x>Link</a><div id=cover></div></body></html>",
            OVERLAY,
        );
        let Reach::Intercepted(i) = page.reach("link") else {
            panic!("{:?}", page.reach("link"))
        };
        assert_eq!(i.cause, InterceptCause::Other);
        assert_eq!(i.by, Some(page.by_id("cover")));
        assert!(i.overlay, "a fixed cover over the viewport: Chrome too");
    }

    #[test]
    fn skip_link_above_the_page_top_is_browser_too() {
        // Parked above the page (`top: -100px`): the click point has no box
        // under it. Chrome can't show it either — browser-too since the
        // P2b vertical row (user decision 2026-11-28); not an overlay.
        let page = Page::new(
            "<html><body><a id=skip href=#main>Skip</a><a id=partly href=#x>Partly</a><p>text</p></body></html>",
            "body { margin: 0; } \
             #skip { position: absolute; left: 0; top: -100px; } \
             #partly { position: absolute; left: 100px; top: -5px; height: 20px; }",
        );
        let Reach::Intercepted(i) = page.reach("skip") else {
            panic!("{:?}", page.reach("skip"))
        };
        assert_eq!((i.cause, i.by), (InterceptCause::Other, None));
        assert!(i.above_page && !i.overlay);
        assert!(page.reach("skip").browser_too());
        // Partly above the top: the visible part is clicked.
        assert_eq!(page.reach("partly"), Reach::Reached);
    }

    #[test]
    fn button_under_an_overlay_is_intercepted() {
        // A small static box drawn over the button by layout order: ours.
        let page = Page::new(
            "<html><body><button id=b>Go</button><div id=over></div></body></html>",
            "#b { width: 100px; height: 30px; } #over { margin-top: -30px; width: 100px; height: 30px; position: relative; }",
        );
        let Reach::Intercepted(i) = page.reach("b") else {
            panic!("{:?}", page.reach("b"))
        };
        assert_eq!(i.cause, InterceptCause::Other);
        assert!(!i.browser_too());
        // The same cover with `pointer-events: none` lets the click through
        // as in Chrome (step 0 counted it as `PointerNoneOverlay`; since 1b
        // the hit test itself passes through, so that cause stays 0).
        let page = Page::new(
            "<html><body><button id=b>Go</button><div id=over></div></body></html>",
            "#b { width: 100px; height: 30px; } #over { margin-top: -30px; width: 100px; height: 30px; position: relative; pointer-events: none; }",
        );
        assert_eq!(page.reach("b"), Reach::Reached);
    }

    #[test]
    fn z_order_interception_is_classified_separately() {
        // The button paints on top (z-index) but comes first in the
        // document. Step 0's DFS hit test picked the later box (`ZOrder`);
        // since 1b-z the hit test walks paint order, so it is reached and
        // that cause stays 0.
        let page = Page::new(
            "<html><body><button id=b>Go</button><div id=later></div></body></html>",
            "#b { position: absolute; left: 0; top: 0; width: 100px; height: 30px; z-index: 2; } \
             #later { position: relative; width: 100px; height: 30px; }",
        );
        assert_eq!(page.reach("b"), Reach::Reached);
    }

    #[test]
    fn label_ancestor_of_the_hit_counts_as_the_target() {
        // Checkbox hack: the box is covered by its own label (and one in
        // another place labels it by `for`) — clicking the label is
        // clicking the checkbox.
        let page = Page::new(
            "<html><body><label id=wrap><input type=checkbox id=c><span id=face>Menu</span></label>\
             <input type=checkbox id=d><label for=d id=lab>Other</label></body></html>",
            "#face { position: absolute; left: 0; top: 0; width: 60px; height: 30px; } \
             #d { position: absolute; left: 100px; top: 0; } \
             #lab { position: absolute; left: 90px; top: 0; width: 60px; height: 30px; }",
        );
        assert_eq!(page.reach("c"), Reach::Reached);
        assert_eq!(page.reach("d"), Reach::Reached);
        // A label for something else is an interception.
        let page = Page::new(
            "<html><body><input type=checkbox id=d><input id=e><label for=e id=lab>Other</label></body></html>",
            "#d { position: absolute; left: 100px; top: 0; } \
             #lab { position: absolute; left: 90px; top: 0; width: 60px; height: 30px; }",
        );
        assert!(
            matches!(page.reach("d"), Reach::Intercepted(_)),
            "{:?}",
            page.reach("d")
        );
    }

    #[test]
    fn reach_uses_the_clamped_scroll_position_click_would_use() {
        // The button sits near the bottom of the page: `click` asks to
        // centre it (420), the engine clamps to the last page-full (300).
        // The hit is tested at 300, exactly where the click would happen.
        let page = Page::new(
            "<html><body><div id=spacer></div><div id=b>Go</div><div id=foot></div></body></html>",
            "body { margin: 0; } #spacer { height: 560px; } #b { height: 20px; } \
             #foot { height: 20px; }",
        );
        let b = page.by_id("b");
        let rect = visible_rect(&page.view.layout_root, b, &page.view.clips)
            .unwrap()
            .unwrap();
        assert_eq!(page.view.document_height, 600.0);
        assert_eq!(scroll_target(rect, 0.0, VIEWPORT.height), Some(420.0));
        let (scroll, point) = page.context().click_position(b).unwrap();
        assert_eq!(scroll, 300.0);
        assert_eq!(
            scroll,
            clamped_scroll(420.0, window_height(VIEWPORT), 600.0)
        );
        // The button's middle (doc y 570) at that scroll, in window terms.
        assert_eq!(point.1, 570.0 - 300.0 + CHROME_HEIGHT);
        assert_eq!(page.reach("b"), Reach::Reached);
    }
}
