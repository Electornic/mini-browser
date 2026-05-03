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
pub mod html;
pub mod js;
pub mod layout;
pub mod navigation;
pub mod net;
pub mod render;
pub mod resource;
pub mod state;
pub mod style;
pub mod window;
