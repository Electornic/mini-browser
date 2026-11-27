//! Cascade layers — `@layer` (compat round 3 step 2.2).
//!
//! The parser unfolds `@layer name { … }` / `@layer { … }` bodies into the
//! sheet's rule list and registers `@layer a, b;` statements; each rule
//! keeps the sheet-local index of its layer ([`super::Rule::layer`]). The
//! sheet records the layer tree it saw ([`super::Stylesheet::layers`]):
//! one entry per name segment, in first-appearance order, so `a.b` is the
//! layer `b` whose parent is `a`.
//!
//! Ranks are document-wide — the same name in two sheets is one layer —
//! and a later statement can still move a layer, so they are assigned per
//! style pass by [`document_layer_ranks`] over every sheet, never inside
//! `css::parse`. A layer first seen inside a `@media`/`@supports` block
//! whose condition is false is ordered anyway, by first appearance (the
//! spec leaves it out of the order; real sites fix the order with a
//! leading statement — docs/compat-round3.md 2.2).

use std::collections::HashMap;
use std::rc::Rc;

use cssparser::{Parser as CssParser, Token};

use super::Stylesheet;
use super::media::MAX_MEDIA_NESTING;

/// The rank of a rule outside every layer: it beats every layer for
/// normal declarations and loses to every layer for `!important` ones.
pub const UNLAYERED: u32 = u32::MAX;

/// One node of a sheet's layer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// The name segment (`b` of `a.b`); `None` for an anonymous
    /// `@layer { … }`, which is a new layer every time.
    pub name: Option<Rc<str>>,
    /// Index of the enclosing layer in the same sheet's list.
    pub parent: Option<u32>,
}

/// A dotted `<layer-name>`, one string per segment.
pub(super) type LayerName = Vec<Rc<str>>;

/// The prelude of `@layer`: an empty list (anonymous block) or a comma
/// list of names. `Err` for anything else — the at-rule is invalid.
/// A name with more segments than the nesting cap is invalid too (real
/// names have one to three; the cap keeps the layer tree's depth bounded).
pub(super) fn parse_prelude(input: &mut CssParser<'_>) -> Result<Vec<LayerName>, ()> {
    let mut names = Vec::new();
    if input.is_exhausted() {
        return Ok(names);
    }
    loop {
        names.push(parse_name(input)?);
        match input.next() {
            Err(_) => return Ok(names),
            Ok(Token::Comma) => continue,
            Ok(_) => return Err(()),
        }
    }
}

fn parse_name(input: &mut CssParser<'_>) -> Result<LayerName, ()> {
    let mut name = vec![segment(input.next().map_err(|_| ())?)?];
    // Segments are joined by `.` with no whitespace around it.
    loop {
        let state = input.state();
        match input.next_including_whitespace() {
            Ok(Token::Delim('.')) => {
                name.push(segment(input.next_including_whitespace().map_err(|_| ())?)?);
                if name.len() > MAX_MEDIA_NESTING {
                    return Err(());
                }
            }
            _ => {
                input.reset(&state);
                return Ok(name);
            }
        }
    }
}

fn segment(token: &Token<'_>) -> Result<Rc<str>, ()> {
    match token {
        // CSS-wide keywords are reserved: the rule is invalid.
        Token::Ident(ident)
            if !super::properties::CSS_WIDE_KEYWORDS
                .contains(&ident.to_ascii_lowercase().as_str()) =>
        {
            Ok(Rc::from(&**ident))
        }
        _ => Err(()),
    }
}

/// What the parser needs to build one sheet's layer list.
#[derive(Default)]
pub(super) struct LayerRegistry {
    pub layers: Vec<Layer>,
    /// (parent, name) → index, so a name seen again is the same layer.
    /// Lookup only: nothing iterates it (determinism).
    lookup: HashMap<(Option<u32>, Rc<str>), u32>,
}

impl LayerRegistry {
    /// The layer `name` under `parent` (created on first sight), or `None`
    /// when the index no longer fits a `u32`.
    pub fn named(&mut self, parent: Option<u32>, name: &[Rc<str>]) -> Option<u32> {
        let mut current = parent;
        for segment in name {
            let key = (current, Rc::clone(segment));
            current = Some(match self.lookup.get(&key) {
                Some(&index) => index,
                None => {
                    let index = self.push(Some(Rc::clone(segment)), current)?;
                    self.lookup.insert(key, index);
                    index
                }
            });
        }
        current
    }

    /// A new anonymous layer under `parent`.
    pub fn anonymous(&mut self, parent: Option<u32>) -> Option<u32> {
        self.push(None, parent)
    }

    fn push(&mut self, name: Option<Rc<str>>, parent: Option<u32>) -> Option<u32> {
        let index = u32::try_from(self.layers.len()).ok()?;
        self.layers.push(Layer { name, parent });
        Some(index)
    }
}

/// Every sheet's layers ranked in one document-wide order:
/// `ranks[sheet][layer]`. Sheets are merged in the order given (same name
/// under the same parent = same layer, first appearance decides its place
/// among its siblings); ranks are the post-order of that tree, so a
/// layer's sublayers rank below the layer's own rules. Linear in the
/// number of layers; no recursion.
pub fn document_layer_ranks(sheets: &[Stylesheet]) -> Vec<Vec<u32>> {
    // Document tree: children of each node (`top` = the root's).
    let mut children: Vec<Vec<usize>> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut lookup: HashMap<(Option<usize>, Rc<str>), usize> = HashMap::new();
    let mut local_to_doc: Vec<Vec<usize>> = Vec::with_capacity(sheets.len());
    for sheet in sheets {
        let mut map: Vec<usize> = Vec::with_capacity(sheet.layers.len());
        for layer in &sheet.layers {
            // A parent always precedes its children in the sheet's list.
            let parent = layer.parent.and_then(|p| map.get(p as usize).copied());
            let existing = layer
                .name
                .as_ref()
                .and_then(|name| lookup.get(&(parent, Rc::clone(name))).copied());
            let node = existing.unwrap_or_else(|| {
                let node = children.len();
                children.push(Vec::new());
                match parent.and_then(|p| children.get_mut(p)) {
                    Some(siblings) => siblings.push(node),
                    None => top.push(node),
                }
                if let Some(name) = &layer.name {
                    lookup.insert((parent, Rc::clone(name)), node);
                }
                node
            });
            map.push(node);
        }
        local_to_doc.push(map);
    }

    // Post-order with an explicit stack of (child list, next position).
    let mut rank = vec![UNLAYERED; children.len()];
    let mut next: u32 = 0;
    let mut stack: Vec<(Option<usize>, usize)> = vec![(None, 0)];
    while let Some((node, position)) = stack.last_mut() {
        let list = match node {
            Some(n) => &children[*n],
            None => &top,
        };
        if let Some(&child) = list.get(*position) {
            *position += 1;
            stack.push((Some(child), 0));
        } else {
            if let Some(n) = *node {
                rank[n] = next;
                next = next.saturating_add(1);
            }
            stack.pop();
        }
    }

    local_to_doc
        .into_iter()
        .map(|map| map.into_iter().map(|node| rank[node]).collect())
        .collect()
}
