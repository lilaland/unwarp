//! Project mirror job — copies `README.md` / `AGENTS.md` / `CLAUDE.md`
//! from project directories under `mirror_source_root` into the vault as
//! read-only `*.mirror.md` files.
//!
//! See PRD §4.8 / TDD §4.2. This commit ships the synchronous scan
//! logic; a follow-up will hook it to either a `notify`-based watcher or
//! a periodic schedule.

pub mod job;

#[allow(unused_imports)]
pub use job::{MirrorError, MirrorJob, MirrorJobReport, MirroredFile};
