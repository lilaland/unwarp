//! Vault configuration — expansion, validation, discovery.
//!
//! `VaultConfig` is the user-supplied vault location + mirror settings,
//! tilde-expanded and normalized to absolute paths. It does not own any
//! filesystem resources; [`VaultManager`](crate::vault::manager::VaultManager)
//! does that.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Default vault root (relative to the user's home directory).
pub const DEFAULT_VAULT_REL_PATH: &str = "Documents/unwarp-vault";

/// Default mirror source root (relative to the user's home directory).
/// The mirror job recursively scans this directory (up to `mirror_max_depth`)
/// for project READMEs / AGENTS / CLAUDE files.
pub const DEFAULT_MIRROR_SOURCE_REL_PATH: &str = "Documents";

/// Default mirror recursion depth.
pub const DEFAULT_MIRROR_MAX_DEPTH: u8 = 2;

/// User-facing vault configuration.
///
/// All paths are absolute (tilde-expanded at construction time). The config
/// itself is plain data; the [`VaultManager`](crate::vault::manager::VaultManager)
/// applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    pub root: PathBuf,
    pub mirror_source_root: PathBuf,
    pub mirror_max_depth: u8,
}

impl VaultConfig {
    /// Build from raw setting strings, expanding `~` against the given home
    /// directory.
    ///
    /// `home_dir` is passed in (not fetched from `dirs::home_dir()`) so the
    /// function is test-friendly and platform-agnostic.
    pub fn from_raw(
        home_dir: &Path,
        vault_path: &str,
        mirror_source_root: &str,
        mirror_max_depth: u8,
    ) -> Result<Self, VaultConfigError> {
        Ok(Self {
            root: expand_tilde(home_dir, vault_path)?,
            mirror_source_root: expand_tilde(home_dir, mirror_source_root)?,
            mirror_max_depth,
        })
    }

    /// Default values relative to a home directory. Useful for first-run
    /// preview before the user picks a custom path.
    pub fn default_for_home(home_dir: &Path) -> Self {
        Self {
            root: home_dir.join(DEFAULT_VAULT_REL_PATH),
            mirror_source_root: home_dir.join(DEFAULT_MIRROR_SOURCE_REL_PATH),
            mirror_max_depth: DEFAULT_MIRROR_MAX_DEPTH,
        }
    }

    /// Returns true if `root` looks like a vault we've previously managed:
    /// the `.unwarp/` directory must exist. Bare paths without it are valid
    /// targets for [`VaultManager::adopt_existing`](crate::vault::manager::VaultManager::adopt_existing)
    /// — adopting just creates the missing subdirs.
    pub fn is_recognized_vault(root: &Path) -> bool {
        root.is_dir() && root.join(".unwarp").is_dir()
    }
}

/// Errors produced while expanding or validating user-supplied vault paths.
#[derive(Debug, Error)]
pub enum VaultConfigError {
    /// `~`-expansion was requested but the path didn't start with `~` (or
    /// `~/`). Should not normally happen since we always pass through
    /// `expand_tilde`, but kept as a typed error for clarity.
    #[error("path `{0}` could not be expanded")]
    Expand(String),

    /// The expanded path is empty.
    #[error("vault path resolved to an empty string")]
    Empty,
}

/// Expand a leading `~` or `~/` against `home_dir`. Other paths are returned
/// verbatim (after a sanity check for emptiness).
///
/// This intentionally avoids `shellexpand` so we don't take a dependency on
/// shell variable resolution; the only expansion that matters here is `~`.
fn expand_tilde(home_dir: &Path, raw: &str) -> Result<PathBuf, VaultConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VaultConfigError::Empty);
    }
    if let Some(rest) = trimmed.strip_prefix('~') {
        let stripped = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\'));
        match stripped {
            Some(after_slash) if !after_slash.is_empty() => Ok(home_dir.join(after_slash)),
            Some(_) => Ok(home_dir.to_owned()),
            // `~user` (no slash) — we don't support per-user expansion.
            None if rest.is_empty() => Ok(home_dir.to_owned()),
            None => Err(VaultConfigError::Expand(raw.to_owned())),
        }
    } else {
        Ok(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/Users/test")
    }

    #[test]
    fn expand_tilde_with_relative_path() {
        let p = expand_tilde(&home(), "~/Documents/notes").unwrap();
        assert_eq!(p, PathBuf::from("/Users/test/Documents/notes"));
    }

    #[test]
    fn expand_tilde_bare() {
        let p = expand_tilde(&home(), "~").unwrap();
        assert_eq!(p, home());
    }

    #[test]
    fn expand_tilde_with_trailing_slash() {
        let p = expand_tilde(&home(), "~/").unwrap();
        assert_eq!(p, home());
    }

    #[test]
    fn absolute_path_unchanged() {
        let p = expand_tilde(&home(), "/etc/hosts").unwrap();
        assert_eq!(p, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn empty_path_is_error() {
        assert!(matches!(
            expand_tilde(&home(), ""),
            Err(VaultConfigError::Empty)
        ));
    }

    #[test]
    fn from_raw_builds_expected_config() {
        let cfg = VaultConfig::from_raw(
            &home(),
            "~/Documents/unwarp-vault",
            "~/Documents",
            2,
        )
        .unwrap();
        assert_eq!(cfg.root, PathBuf::from("/Users/test/Documents/unwarp-vault"));
        assert_eq!(cfg.mirror_source_root, PathBuf::from("/Users/test/Documents"));
        assert_eq!(cfg.mirror_max_depth, 2);
    }

    #[test]
    fn default_for_home_matches_constants() {
        let cfg = VaultConfig::default_for_home(&home());
        assert_eq!(cfg.root, home().join(DEFAULT_VAULT_REL_PATH));
        assert_eq!(
            cfg.mirror_source_root,
            home().join(DEFAULT_MIRROR_SOURCE_REL_PATH)
        );
        assert_eq!(cfg.mirror_max_depth, DEFAULT_MIRROR_MAX_DEPTH);
    }

    #[test]
    fn is_recognized_vault_requires_unwarp_subdir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!VaultConfig::is_recognized_vault(dir.path()));
        std::fs::create_dir(dir.path().join(".unwarp")).unwrap();
        assert!(VaultConfig::is_recognized_vault(dir.path()));
    }
}
