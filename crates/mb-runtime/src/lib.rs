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
pub(crate) use mb_dom::{css, dom, dom_select, html, style};
pub(crate) use mb_engine::{chrome, display_list, layout, render};
pub(crate) use mb_engine::input as window;

pub mod js;
pub mod navigation;
pub mod net;
pub mod resource;
pub mod state;
