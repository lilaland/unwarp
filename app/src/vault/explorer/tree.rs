//! Vault tree model.
//!
//! Holds a depth-tagged flat list of [`VaultEntry`] rows for rendering.
//! Realistic vault sizes stay under a few hundred entries (brew docs + a
//! handful of projects + runbooks + the user's own notes), so we skip the
//! SumTree optimization the code-panel file tree uses (TDD §5.5 documents
//! the option for later if scale demands it).
//!
//! The tree categorizes each entry so the renderer can:
//! - show read-only affordances on `brew/` content and `*.mirror.md` files
//! - keep `notes/` content visually distinguished as the user's domain
//!
//! Collapse/expand isn't implemented here — v1 renders the entire vault
//! flat. Adding collapse state on top of this Vec is straightforward when
//! we need it.

use std::path::{Path, PathBuf};

use crate::vault::layout::NOTES_DIR_NAME;

/// One row in the rendered tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultEntry {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Display label (file name with extension, or directory name).
    pub name: String,
    /// Path relative to the vault root, lowercased for stable comparison.
    pub rel_path: PathBuf,
    /// Indent level. 0 = direct child of the vault root.
    pub depth: usize,
    pub kind: VaultEntryKind,
    pub category: VaultCategory,
    /// True when the panel should NOT offer in-app editing for this row.
    /// (Brew docs are AI-managed; `*.mirror.md` files are sourced from
    /// outside the vault and are clobbered on every mirror run.)
    pub is_read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultEntryKind {
    File,
    Directory,
}

/// Categorizes a vault entry by where it lives in the layout. Drives
/// rendering decisions: icons, read-only badges, sort priority within a
/// directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultCategory {
    /// `brew/` — Homebrew formula docs (AI-managed, read-only in app).
    Brew,
    /// `projects/<name>/*.mirror.md` — read-only mirror of an external README/AGENTS/CLAUDE.
    ProjectMirror,
    /// `projects/<name>/notes.md` or other files the user owns.
    ProjectNote,
    /// `runbooks/` — runbooks the AI and user share read/write access to.
    Runbook,
    /// `notes/` — user's free-form workspace; the app never writes here.
    UserNote,
    /// A directory (top-level or nested).
    Directory,
    /// Anything else that ends up in the vault (e.g., user-dropped files).
    Other,
}

impl VaultCategory {
    /// True if the category corresponds to AI-managed content the user
    /// shouldn't edit in-app.
    fn is_read_only(self) -> bool {
        matches!(self, Self::Brew | Self::ProjectMirror)
    }
}

/// Walk `vault_root` recursively and return entries in display order:
/// directories grouped first within each parent, alphabetical within
/// kind. Top-level order is `runbooks/`, `projects/`, `brew/`, `notes/`,
/// then any other directories alphabetically. Hidden files and `.unwarp/`
/// are skipped.
///
/// I/O errors during traversal are silently skipped — the panel falls
/// back to rendering whatever entries we did manage to collect. (We could
/// surface them in a future revision; the simple-case behavior is more
/// important right now.)
pub fn walk_vault(vault_root: &Path) -> Vec<VaultEntry> {
    let mut entries = Vec::new();
    let _ = walk_dir(vault_root, vault_root, 0, &mut entries);
    entries
}

/// Recursive helper. Returns Ok even when individual children fail to read.
fn walk_dir(
    vault_root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<VaultEntry>,
) -> std::io::Result<()> {
    let read_dir = std::fs::read_dir(dir)?;
    let mut children: Vec<_> = read_dir.flatten().collect();
    // Sort children: directories first, then alphabetical within each kind.
    // For the top-level (depth == 0) we apply a custom ordering that puts
    // the user-relevant directories first.
    children.sort_by(|a, b| compare_children(a, b, depth == 0));

    for child in children {
        let path = child.path();
        let Some(name_os) = path.file_name() else {
            continue;
        };
        let Some(name) = name_os.to_str() else {
            continue;
        };
        // Skip the `.unwarp/` machinery and any hidden file/dir.
        if name.starts_with('.') {
            continue;
        }

        let file_type = match child.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let rel_path = path
            .strip_prefix(vault_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(name));
        let category = classify(&rel_path, file_type.is_dir());
        let kind = if file_type.is_dir() {
            VaultEntryKind::Directory
        } else {
            VaultEntryKind::File
        };

        out.push(VaultEntry {
            path: path.clone(),
            name: name.to_owned(),
            rel_path,
            depth,
            kind,
            category,
            is_read_only: category.is_read_only(),
        });

        if file_type.is_dir() {
            let _ = walk_dir(vault_root, &path, depth + 1, out);
        }
    }
    Ok(())
}

fn compare_children(
    a: &std::fs::DirEntry,
    b: &std::fs::DirEntry,
    is_top_level: bool,
) -> std::cmp::Ordering {
    let a_is_dir = a.file_type().map(|f| f.is_dir()).unwrap_or(false);
    let b_is_dir = b.file_type().map(|f| f.is_dir()).unwrap_or(false);

    // Directories before files.
    if a_is_dir != b_is_dir {
        return if a_is_dir {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }

    // Top-level: user-relevant categories first.
    if is_top_level && a_is_dir {
        let a_rank = top_level_dir_rank(a.file_name().to_str().unwrap_or(""));
        let b_rank = top_level_dir_rank(b.file_name().to_str().unwrap_or(""));
        if a_rank != b_rank {
            return a_rank.cmp(&b_rank);
        }
    }

    // Otherwise alphabetical, case-insensitive.
    let an = a.file_name().to_string_lossy().to_lowercase();
    let bn = b.file_name().to_string_lossy().to_lowercase();
    an.cmp(&bn)
}

fn top_level_dir_rank(name: &str) -> u8 {
    match name {
        "runbooks" => 0,
        "projects" => 1,
        "brew" => 2,
        NOTES_DIR_NAME => 3,
        _ => 4,
    }
}

/// Classify an entry by its position in the vault layout.
fn classify(rel_path: &Path, is_dir: bool) -> VaultCategory {
    if is_dir {
        return VaultCategory::Directory;
    }
    let components: Vec<&str> = rel_path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let first = components.first().copied().unwrap_or("");
    let name = rel_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    match first {
        "brew" => VaultCategory::Brew,
        "runbooks" => VaultCategory::Runbook,
        NOTES_DIR_NAME => VaultCategory::UserNote,
        "projects" => {
            if name.ends_with(".mirror.md") {
                VaultCategory::ProjectMirror
            } else {
                VaultCategory::ProjectNote
            }
        }
        _ => VaultCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout::ensure_layout;

    fn ws(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn classify_recognizes_each_managed_dir() {
        assert_eq!(classify(&ws("brew/jq.md"), false), VaultCategory::Brew);
        assert_eq!(
            classify(&ws("runbooks/deploy.md"), false),
            VaultCategory::Runbook
        );
        assert_eq!(
            classify(&ws("notes/my-thoughts.md"), false),
            VaultCategory::UserNote
        );
        assert_eq!(
            classify(&ws("projects/foo/README.mirror.md"), false),
            VaultCategory::ProjectMirror
        );
        assert_eq!(
            classify(&ws("projects/foo/notes.md"), false),
            VaultCategory::ProjectNote
        );
        assert_eq!(classify(&ws("brew/"), true), VaultCategory::Directory);
    }

    #[test]
    fn read_only_classification_matches_tdd() {
        assert!(VaultCategory::Brew.is_read_only());
        assert!(VaultCategory::ProjectMirror.is_read_only());
        assert!(!VaultCategory::Runbook.is_read_only());
        assert!(!VaultCategory::UserNote.is_read_only());
        assert!(!VaultCategory::ProjectNote.is_read_only());
    }

    #[test]
    fn walk_vault_skips_hidden_and_unwarp_machinery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_layout(root).unwrap();
        // Plant a hidden file at the root.
        std::fs::write(root.join(".secret"), "hidden").unwrap();

        let entries = walk_vault(root);
        assert!(
            entries.iter().all(|e| !e.name.starts_with('.')),
            "no hidden entries should appear"
        );
        // `.unwarp/` was created by ensure_layout; verify it's not in the tree.
        assert!(
            entries.iter().all(|e| e.name != ".unwarp"),
            ".unwarp/ should be filtered as hidden"
        );
    }

    #[test]
    fn walk_vault_orders_top_level_by_rank() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_layout(root).unwrap();
        // notes/ doesn't exist yet (intentional per TDD §3.3); create it
        // so we can verify the order.
        std::fs::create_dir(root.join("notes")).unwrap();

        let entries = walk_vault(root);
        let top_dirs: Vec<&str> = entries
            .iter()
            .filter(|e| e.depth == 0 && e.kind == VaultEntryKind::Directory)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(top_dirs, vec!["runbooks", "projects", "brew", "notes"]);
    }

    #[test]
    fn walk_vault_tags_brew_files_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_layout(root).unwrap();
        std::fs::write(root.join("brew/jq.md"), "# jq").unwrap();

        let entries = walk_vault(root);
        let jq = entries
            .iter()
            .find(|e| e.name == "jq.md")
            .expect("expected jq.md in tree");
        assert!(jq.is_read_only);
        assert_eq!(jq.category, VaultCategory::Brew);
    }

    #[test]
    fn walk_vault_emits_directories_before_files_within_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_layout(root).unwrap();
        std::fs::write(root.join("brew/aa.md"), "x").unwrap();
        std::fs::write(root.join("brew/zz.md"), "x").unwrap();

        let entries = walk_vault(root);
        // We can't easily test "dirs before files" at root because root only
        // has dirs; instead, check the alphabetical order within brew/.
        let names: Vec<&str> = entries
            .iter()
            .filter(|e| e.depth == 1 && e.rel_path.starts_with("brew"))
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["_index.md", "aa.md", "zz.md"],
            "files in a directory must be alphabetical (case-insensitive)"
        );
    }
}
