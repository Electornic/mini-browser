// This crate is organized by browser pipeline stage.
// Reading the files in roughly this order makes the code easiest to follow:
// DOM -> HTML/CSS parse -> style -> layout -> render -> window/app.
//
// The top-level `state` module owns the per-frame loop driver
// (`BrowserState`); `display_list` owns the styled-tree -> paint-commands
// translation it calls into every frame. `chrome` and `navigation` are the
// two earlier splits that lifted UI painting and the document loader out of
// `main.rs`.
pub mod chrome;
pub mod css;
pub mod display_list;
pub mod dom;
pub mod dom_select;
pub mod html;
pub mod js;
// Phase 4.8 staging area: rquickjs-backed JsRuntime under construction.
// 4.8a–d build it up alongside the boa version; 4.8e flips callers over
// and removes `js`. Unused until then — `dead_code` keeps the build quiet.
#[allow(dead_code)]
pub mod js_quick;
pub mod layout;
pub mod navigation;
pub mod net;
pub mod render;
pub mod resource;
pub mod state;
pub mod style;
pub mod window;
