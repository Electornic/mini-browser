// Browser orchestrator + JS bridge + IO. Sits above the engine in the dep
// graph: drives the per-frame loop in `state::BrowserState`, owns the
// rquickjs runtime and its DOM bridge in `js`, and wraps `ureq` for fetch
// in `net`.
//
// `pub(crate) use` of mb-dom / mb-engine modules keeps the original
// `crate::css::Color` / `crate::layout::Rect` paths resolving inside the
// moved files. `crate::window` is aliased to mb-engine's `input` module so
// state.rs's `window::WindowInput` references keep compiling — the actual
// winit driver still lives in the root `mini-browser` crate (4.9e).
// Re-exports of mb-dom + mb-engine surfaces are `pub` so the integration
// tests in `tests/` (and any external embedder) can reach the styled-tree
// primitives, paint commands, and chrome layout from a single
// `mb_runtime::*` namespace. Inside this crate they double as the
// shorthand `crate::css::Color` / `crate::layout::Rect` paths the moved
// files expect.
pub use mb_dom::{css, dom, dom_select, html, style};
pub use mb_engine::{chrome, display_list, input, layout, render};

pub mod async_runtime;
pub mod js;
pub mod navigation;
pub mod net;
pub mod resource;
pub mod state;
