// This crate is organized by browser pipeline stage.
// Reading the files in roughly this order makes the code easiest to follow:
// DOM -> HTML/CSS parse -> style -> layout -> render -> window/app.
//
// The top-level `state` module owns the per-frame loop driver
// (`BrowserState`); `display_list` owns the styled-tree -> paint-commands
// translation it calls into every frame. `chrome` and `navigation` are the
// two earlier splits that lifted UI painting and the document loader out of
// `main.rs`.
//
// Phase 4.9 is splitting the crate into a workspace; pure-data parsers
// (dom/css/style/dom_select/html/resource) now live in `mb-dom` and are
// re-exported here so existing `crate::dom::…` paths keep working until the
// remaining phases land.
pub use mb_dom::{css, dom, dom_select, html, style};

pub mod chrome;
pub mod display_list;
pub mod js;
pub mod layout;
pub mod navigation;
pub mod net;
pub mod render;
pub mod resource;
pub mod state;
pub mod window;
