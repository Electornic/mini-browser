// This crate is organized by browser pipeline stage.
// Reading the files in roughly this order makes the code easiest to follow:
// DOM -> HTML/CSS parse -> style -> layout -> render -> window/app.
//
// Phase 4.9 is splitting the crate into a workspace; pure-data parsers
// (dom/css/style/dom_select/html) live in `mb-dom`, and the layout/paint
// pipeline (chrome/display_list/layout/render plus the shared font system)
// lives in `mb-engine`. Both are re-exported here so existing
// `crate::dom::…` / `crate::layout::…` paths keep working until the
// remaining sub-phases land.
pub use mb_dom::{css, dom, dom_select, html, style};
pub use mb_engine::{chrome, display_list, layout, render};

pub mod js;
pub mod navigation;
pub mod net;
pub mod resource;
pub mod state;
pub mod window;
