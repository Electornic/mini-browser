// Single-process tokio runtime that BrowserState dispatches blocking I/O
// onto via `spawn_blocking`. Phase 5.8a only ships the refresh path through
// here; navigate / navigate_to_href stay synchronous until 5.8b proves out
// the test-driving pattern. The runtime is lazily created on first access
// so unit tests that never touch the network never spin up worker threads.
//
// Multi-threaded flavour is intentional: blocking I/O lives on the
// `spawn_blocking` thread pool, separate from the (currently unused)
// async worker pool. We pay the runtime overhead once per process.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn handle() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to start tokio runtime"))
}
