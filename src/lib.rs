// This crate is organized by browser pipeline stage.
// Reading the files in roughly this order makes the code easiest to follow:
// DOM -> HTML/CSS parse -> style -> layout -> render -> window/app.
pub mod css;
pub mod dom;
pub mod html;
pub mod js;
pub mod layout;
pub mod net;
pub mod render;
pub mod resource;
pub mod style;
pub mod window;
