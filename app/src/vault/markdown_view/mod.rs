//! Vault markdown viewer — renders a single `.md` file from the vault as
//! a read-only View backed by OpenWarp's `markdown_parser` crate +
//! `FormattedTextElement`. Editor mode is a separate follow-up; v1 viewer
//! is read-only.
//!
//! Design: see `unwarp-tdd-vault-rag.md` §5.3 (the "renderer-only path").

pub mod viewer;

#[allow(unused_imports)]
pub use viewer::{VaultMarkdownViewer, VaultMarkdownViewerAction, ViewerError};
