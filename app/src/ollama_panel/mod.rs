//! Left-sidebar panel that shows the state of the local Ollama server.
//!
//! v1 scope: read-only display of running + pulled models, 2s polling.
//! Pull/unload UI deferred to v1.1.
//!
//! Design: see `unwarp-tdd-ollama-panel.md`.

pub mod client;
pub mod panel;

pub use panel::OllamaPanel;
