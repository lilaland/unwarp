//! Vault explorer — left-sidebar panel that lists vault files and routes
//! clicks to the markdown viewer / editor.
//!
//! Replaces (in the LeftPanelView slot) the now-hidden Drive panel from
//! upstream Warp / OpenWarp.
//!
//! Build-out is split across three commits per Phase 2 plan:
//! - B-1 (this commit): stub view, panel slot, button, empty state
//! - B-2: SumTree-backed tree population from a directory walk
//! - B-3: click-to-open routing and filesystem-watcher refresh

pub mod panel;
pub mod tree;

#[allow(unused_imports)]
pub use panel::{VaultPanel, VaultPanelAction};
#[allow(unused_imports)]
pub use tree::{VaultCategory, VaultEntry, VaultEntryKind};
