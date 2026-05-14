//! unwarp vault — Obsidian-compatible markdown vault for runbooks, project
//! mirrors, and brew docs.
//!
//! Design: see `unwarp-tdd-vault-rag.md` §3.
//!
//! Phase 2 scope (this commit):
//! - [`config`] — `VaultConfig` and discovery / validation
//! - [`layout`] — directory layout enforcement (creates `brew/`, `projects/`,
//!   `runbooks/`, `.unwarp/`; never touches `notes/`)
//! - [`lock`]   — `.unwarp/lock` PID-file with stale detection
//! - [`manager`] — `VaultManager` singleton (state machine, settings glue)
//!
//! Phase 2 follow-ups (separate commits): vault explorer panel, markdown
//! viewer/editor, mirror job, brew docs job.
//!
//! Phase 3 will hook the vault file watcher into the RAG indexer.

pub mod brew;
pub mod config;
pub mod explorer;
pub mod layout;
pub mod lock;
pub mod manager;
pub mod markdown_view;

// Re-exports — VaultManagerEvent and VaultState are consumed by the
// vault explorer panel landing in the next commit; allow until then.
#[allow(unused_imports)]
pub use config::{VaultConfig, VaultConfigError};
#[allow(unused_imports)]
pub use layout::{ensure_layout, is_under_notes, LayoutError, NOTES_DIR_NAME};
#[allow(unused_imports)]
pub use lock::{VaultLock, VaultLockError};
#[allow(unused_imports)]
pub use manager::{VaultManager, VaultManagerEvent, VaultState};
