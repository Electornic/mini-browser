// Pure-data layer of the browser pipeline: parsers and styled-tree primitives
// with zero I/O dependencies.
//
//   html → dom               (HTML5 parser → arena tree)
//   css                      (CSS parser → stylesheet AST)
//   style → css              (cascade + computed values)
//   dom_select → {css, dom}  (selector matching)
//
// `resource` belongs here logically but reaches into `net` for fetching, so it
// stays in the root crate until 4.9d when net + the loader both move to
// mb-runtime. Consumers depend on this crate via path = "crates/mb-dom".
pub mod css;
pub mod dom;
pub mod dom_select;
pub mod html;
pub mod resource;
pub mod style;
pub mod url;
