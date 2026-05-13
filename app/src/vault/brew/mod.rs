//! Brew docs job — generates one markdown file per installed Homebrew
//! formula under `<vault>/brew/`. Manual trigger only in v1; UI button +
//! palette command land in a follow-up commit.
//!
//! Design: PRD §4.8.

pub mod job;

#[allow(unused_imports)]
pub use job::{BrewError, BrewJob, BrewJobReport};
