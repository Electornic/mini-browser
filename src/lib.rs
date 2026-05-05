// Phase 4.9 split this crate into a workspace; what remains here is the
// binary entry point (`main.rs`) plus the winit-driven `window` module.
// All the browser pipeline modules now live in their workspace crates and
// are re-exported below so existing `mini_browser::dom::…` paths
// (especially the integration tests under `tests/`) keep resolving.
pub use mb_dom::{css, dom, dom_select, html, style};
pub use mb_engine::{chrome, display_list, layout, render};
pub use mb_runtime::{js, navigation, net, resource, state};

pub mod window;
