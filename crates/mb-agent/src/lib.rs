// Agent-facing interface to the browser engine (docs/concept.md).
//
// G0 ships the observation half: turn a laid-out document into the text an
// LLM agent reads — a numbered list of interactive elements plus the visible
// page text, with browser-computed facts kept apart from page-authored
// strings. Actions, settle, and logging (G1+) build on the same element ids.
//
// Two rules shape everything in here:
//   * Deterministic output. Same document + same layout → byte-identical
//     text. Everything is ordered by document order; no HashMap iteration
//     leaks into the output.
//   * Page strings are untrusted. Every string the page authored is quoted
//     and stripped of control characters, so a page can't forge a line that
//     looks like browser output (e.g. a button named "\n# browser").

#![deny(clippy::print_stdout)]

mod browser;
mod error;
mod index;
mod observation;
pub mod reach;
mod role;
mod text;

pub use browser::{
    AgentBrowser, AgentConfig, ClockMode, PendingWait, SUPPORTED_KEYS, Screenshot, WaitCondition,
    is_local_dev_host,
};
pub use error::{AgentError, ClipHint, Interceptor, NotInteractableReason, PageLink};
pub use mb_runtime::state::policy::ApproveMode;
pub use observation::{
    Changes, ElementObservation, ElementValue, Flag, Observation, ObserveInput, ObserveOptions,
    Scope, ValueChange, observe,
};
pub use role::Role;
