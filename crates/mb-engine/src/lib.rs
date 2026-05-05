// Layout + paint pipeline. Consumes `mb-dom`'s styled tree and produces paint
// commands the shell can rasterise. The runtime layer (`mb-runtime`) drives
// it through `display_list::build_document_view` per frame.
//
//   layout   = box-tree builder (taffy-backed; legacy block/inline/flex/grid
//              algorithms still cover boundary cases)
//   render   = display-list paint commands + tiny-skia / cosmic-text rasteriser
//   chrome   = address-bar + back/forward UI as paint commands
//   display_list = per-frame view builder (DocumentView) + hit testing
//   font_system  = shared cosmic-text FontSystem + swash glyph cache
//   input        = per-frame WindowInput shape (winit driver lives in the shell)
//
// `pub(crate) use mb_dom::{...}` lets the moved modules keep the original
// `crate::css::Color` / `crate::dom::NodeType` import paths without rewriting
// every file. mb-engine does NOT re-export these to its public API surface
// — consumers go through mb-dom directly.
pub(crate) use mb_dom::{css, dom, resource, style, url};
#[cfg(test)]
pub(crate) use mb_dom::html;

pub mod chrome;
pub mod display_list;
pub mod font_system;
pub mod input;
pub mod layout;
pub mod render;
