//! Mirror-job implementation.
//!
//! Project detection: under `source_root`, a directory at depth ≤
//! `max_depth` is a "project" if it contains either:
//! - a `.git/` subdirectory, or
//! - a `README.md` file
//!
//! For each detected project, [`scan_once`](MirrorJob::scan_once) copies
//! the present subset of `{README.md, AGENTS.md, CLAUDE.md}` to
//! `<vault>/projects/<project_name>/<filename>.mirror.md`.
//!
//! After mirroring, any `*.mirror.md` file in the vault that has no
//! corresponding source project (or whose source file no longer exists)
//! is deleted. Non-`.mirror.md` files in `projects/<name>/` are
//! preserved (the user's own `notes.md` etc.).
//!
//! The job is idempotent: per-file writes are skipped when the on-disk
//! content matches via SHA-256, so the indexer (Phase 3) doesn't see
//! spurious mtimes.
//!
//! No filesystem watcher is wired here — call `scan_once` from a
//! periodic task or a `notify` event handler in a follow-up commit.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

const MIRROR_SUFFIX: &str = ".mirror.md";
const MIRROR_GENERATED_HEADER: &str =
    "<!-- AUTO-GENERATED mirror by unwarp. Edit the source file instead; manual changes here will be overwritten. -->\n\n";

/// File names we mirror (in display priority — README first, then AGENTS,
/// then CLAUDE).
const SOURCE_FILE_NAMES: &[&str] = &["README.md", "AGENTS.md", "CLAUDE.md"];

#[derive(Debug, Error)]
pub enum MirrorError {
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("source root does not exist: {0}")]
    SourceMissing(String),

    #[error("vault projects directory does not exist: {0}")]
    VaultProjectsMissing(String),
}

#[derive(Debug, Default, Clone)]
pub struct MirrorJobReport {
    pub written: Vec<MirroredFile>,
    pub skipped: Vec<MirroredFile>,
    pub deleted: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirroredFile {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub project_name: String,
}

/// Mirror job, parameterized by vault root, source root, and max depth.
#[derive(Debug, Clone)]
pub struct MirrorJob {
    vault_root: PathBuf,
    source_root: PathBuf,
    max_depth: u8,
}

impl MirrorJob {
    pub fn new(vault_root: PathBuf, source_root: PathBuf, max_depth: u8) -> Self {
        Self {
            vault_root,
            source_root,
            max_depth,
        }
    }

    /// Mirror a single changed source file (called from the filesystem watcher).
    ///
    /// `source_path` must be an absolute path to one of the mirror source files
    /// (README.md, AGENTS.md, CLAUDE.md). The project name is inferred from its
    /// parent directory relative to `source_root`.
    pub fn copy_file(&self, source_path: &Path) -> Result<(), MirrorError> {
        let project_dir = source_path
            .parent()
            .ok_or_else(|| MirrorError::Io {
                path: source_path.display().to_string(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"),
            })?;

        let relative = project_dir.strip_prefix(&self.source_root).unwrap_or(project_dir);
        let project_name = project_display_name(
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown"),
            relative,
        );

        let file_name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let dest_dir = self.vault_root.join("projects").join(&project_name);
        let dest = dest_dir.join(mirror_filename(file_name));

        // If source has vanished between notify and read, treat as removal.
        if !source_path.exists() {
            return self.handle_remove(source_path);
        }

        mirror_one(source_path, &dest).map(|_| ())
    }

    /// Remove the vault mirror corresponding to a deleted source file.
    pub fn handle_remove(&self, source_path: &Path) -> Result<(), MirrorError> {
        let project_dir = source_path
            .parent()
            .ok_or_else(|| MirrorError::Io {
                path: source_path.display().to_string(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"),
            })?;

        let relative = project_dir.strip_prefix(&self.source_root).unwrap_or(project_dir);
        let project_name = project_display_name(
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown"),
            relative,
        );

        let file_name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let dest = self
            .vault_root
            .join("projects")
            .join(&project_name)
            .join(mirror_filename(file_name));

        if dest.exists() {
            std::fs::remove_file(&dest).map_err(|e| MirrorError::Io {
                path: dest.display().to_string(),
                source: e,
            })?;
        }
        Ok(())
    }

    /// Run a single full scan of the source tree, mirroring all detected
    /// project files and sweeping stale `.mirror.md` entries.
    ///
    /// Per-file errors are collected in the report rather than aborting.
    pub fn scan_once(&self) -> Result<MirrorJobReport, MirrorError> {
        if !self.source_root.is_dir() {
            return Err(MirrorError::SourceMissing(
                self.source_root.display().to_string(),
            ));
        }
        let projects_root = self.vault_root.join("projects");
        if !projects_root.is_dir() {
            return Err(MirrorError::VaultProjectsMissing(
                projects_root.display().to_string(),
            ));
        }

        let mut report = MirrorJobReport::default();
        let projects = find_projects(&self.source_root, self.max_depth);
        let mut mirrored_dests: HashSet<PathBuf> = HashSet::new();

        for project in &projects {
            for file_name in SOURCE_FILE_NAMES {
                let source = project.path.join(file_name);
                if !source.is_file() {
                    continue;
                }
                let project_dir = projects_root.join(&project.name);
                let dest = project_dir.join(mirror_filename(file_name));
                let mirrored = MirroredFile {
                    source: source.clone(),
                    dest: dest.clone(),
                    project_name: project.name.clone(),
                };

                match mirror_one(&source, &dest) {
                    Ok(WriteOutcome::Written) => {
                        mirrored_dests.insert(dest);
                        report.written.push(mirrored);
                    }
                    Ok(WriteOutcome::Unchanged) => {
                        mirrored_dests.insert(dest);
                        report.skipped.push(mirrored);
                    }
                    Err(e) => {
                        report.errors.push((source, e.to_string()));
                    }
                }
            }
        }

        // Sweep stale mirror files. Walk projects_root/*/*.mirror.md and
        // delete any whose path isn't in `mirrored_dests`. Non-mirror files
        // (user-authored notes.md etc.) and other extensions are left alone.
        match sweep_stale_mirrors(&projects_root, &mirrored_dests) {
            Ok(deleted) => report.deleted = deleted,
            Err(e) => report
                .errors
                .push((projects_root.clone(), e.to_string())),
        }

        Ok(report)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DetectedProject {
    name: String,
    path: PathBuf,
}

/// Walk `source_root` to depth ≤ `max_depth`, returning every directory
/// that looks like a project (has `.git/` or `README.md`).
fn find_projects(source_root: &Path, max_depth: u8) -> Vec<DetectedProject> {
    let mut out = Vec::new();
    let _ = walk_for_projects(source_root, source_root, max_depth, &mut out);
    out
}

fn walk_for_projects(
    source_root: &Path,
    dir: &Path,
    max_depth: u8,
    out: &mut Vec<DetectedProject>,
) -> io::Result<()> {
    if max_depth == 0 {
        return Ok(());
    }
    let read_dir = std::fs::read_dir(dir)?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name_os) = path.file_name() else {
            continue;
        };
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        if is_project_dir(&path) {
            let relative = path.strip_prefix(source_root).unwrap_or(&path);
            out.push(DetectedProject {
                name: project_display_name(name, relative),
                path: path.clone(),
            });
        }
        // Recurse regardless of whether this directory was a project: a
        // monorepo at depth 1 may contain sub-projects at depth 2.
        let _ = walk_for_projects(source_root, &path, max_depth.saturating_sub(1), out);
    }
    Ok(())
}

fn is_project_dir(path: &Path) -> bool {
    path.join(".git").is_dir() || path.join("README.md").is_file()
}

/// Display name for a detected project. Top-level projects use their bare
/// directory name; nested projects flatten the relative path with `-` so
/// we don't collide (e.g., `mono/services/auth` → `mono-services-auth`).
fn project_display_name(dir_name: &str, relative: &Path) -> String {
    let segments: Vec<&str> = relative
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if segments.len() <= 1 {
        dir_name.to_owned()
    } else {
        segments.join("-")
    }
}

fn mirror_filename(source_file_name: &str) -> String {
    let stem = source_file_name.strip_suffix(".md").unwrap_or(source_file_name);
    format!("{stem}{MIRROR_SUFFIX}")
}

enum WriteOutcome {
    Written,
    Unchanged,
}

/// Mirror a single source file to its destination, prefixing the
/// generated header. Creates the destination directory if needed.
fn mirror_one(source: &Path, dest: &Path) -> Result<WriteOutcome, MirrorError> {
    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| MirrorError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }
    }
    let raw = std::fs::read_to_string(source).map_err(|e| MirrorError::Io {
        path: source.display().to_string(),
        source: e,
    })?;
    let new_content = format!("{MIRROR_GENERATED_HEADER}{raw}");
    let new_hash = sha256_str(&new_content);

    if let Ok(existing) = std::fs::read_to_string(dest) {
        if sha256_str(&existing) == new_hash {
            return Ok(WriteOutcome::Unchanged);
        }
    }
    std::fs::write(dest, new_content).map_err(|e| MirrorError::Io {
        path: dest.display().to_string(),
        source: e,
    })?;
    Ok(WriteOutcome::Written)
}

fn sha256_str(s: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().into()
}

/// Walk `projects_root/*/*.mirror.md` and delete files not in `keep`.
/// Returns the deleted paths.
fn sweep_stale_mirrors(
    projects_root: &Path,
    keep: &HashSet<PathBuf>,
) -> Result<Vec<PathBuf>, MirrorError> {
    let mut deleted = Vec::new();
    let dirs = match std::fs::read_dir(projects_root) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(deleted),
        Err(e) => {
            return Err(MirrorError::Io {
                path: projects_root.display().to_string(),
                source: e,
            })
        }
    };
    for project_entry in dirs.flatten() {
        let project_dir = project_entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let files = match std::fs::read_dir(&project_dir) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(MIRROR_SUFFIX) {
                continue;
            }
            if keep.contains(&path) {
                continue;
            }
            // Only delete files that start with our generated header — never
            // touch user content even if it accidentally got named *.mirror.md.
            if !is_generated_mirror(&path) {
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                return Err(MirrorError::Io {
                    path: path.display().to_string(),
                    source: e,
                });
            }
            deleted.push(path);
        }
    }
    Ok(deleted)
}

fn is_generated_mirror(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.starts_with(MIRROR_GENERATED_HEADER.trim_end()),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout::ensure_layout;

    /// Build a complete `(vault_root, source_root)` pair on the same tempdir
    /// and return both rooted paths along with the tempdir guard.
    fn make_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        let source = dir.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        ensure_layout(&vault).unwrap();
        (dir, vault, source)
    }

    /// Create a fake project directory with the given marker files.
    fn make_project(source_root: &Path, rel: &str, files: &[(&str, &str)]) {
        let dir = source_root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
    }

    #[test]
    fn is_project_dir_recognizes_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(is_project_dir(dir.path()));
    }

    #[test]
    fn is_project_dir_recognizes_readme() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        assert!(is_project_dir(dir.path()));
    }

    #[test]
    fn is_project_dir_rejects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_project_dir(dir.path()));
    }

    #[test]
    fn mirror_filename_appends_suffix() {
        assert_eq!(mirror_filename("README.md"), "README.mirror.md");
        assert_eq!(mirror_filename("AGENTS.md"), "AGENTS.mirror.md");
        assert_eq!(mirror_filename("CLAUDE.md"), "CLAUDE.mirror.md");
    }

    #[test]
    fn project_display_name_flattens_nested_paths() {
        assert_eq!(
            project_display_name("auth", Path::new("mono/services/auth")),
            "mono-services-auth"
        );
        assert_eq!(project_display_name("foo", Path::new("foo")), "foo");
    }

    #[test]
    fn scan_once_mirrors_single_project_readme() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "myproj", &[("README.md", "hello world")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        let report = job.scan_once().unwrap();
        assert_eq!(report.written.len(), 1);
        assert!(report.errors.is_empty());
        let mirror_path = vault.join("projects/myproj/README.mirror.md");
        assert!(mirror_path.is_file());
        let body = std::fs::read_to_string(&mirror_path).unwrap();
        assert!(body.starts_with(MIRROR_GENERATED_HEADER));
        assert!(body.contains("hello world"));
    }

    #[test]
    fn scan_once_mirrors_all_three_marker_files() {
        let (_g, vault, source) = make_dirs();
        make_project(
            &source,
            "x",
            &[
                ("README.md", "rm"),
                ("AGENTS.md", "ag"),
                ("CLAUDE.md", "cl"),
            ],
        );
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        let report = job.scan_once().unwrap();
        assert_eq!(report.written.len(), 3);
        assert!(vault.join("projects/x/README.mirror.md").is_file());
        assert!(vault.join("projects/x/AGENTS.mirror.md").is_file());
        assert!(vault.join("projects/x/CLAUDE.mirror.md").is_file());
    }

    #[test]
    fn scan_once_recurses_within_max_depth() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "nested/inner", &[("README.md", "x")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 3);
        let report = job.scan_once().unwrap();
        assert_eq!(report.written.len(), 1, "expected to find nested project");
        let mirror = vault.join("projects/nested-inner/README.mirror.md");
        assert!(mirror.is_file());
    }

    #[test]
    fn scan_once_respects_max_depth() {
        let (_g, vault, source) = make_dirs();
        // Project is at depth 3, but max_depth = 1 so we should miss it.
        make_project(&source, "a/b/c", &[("README.md", "x")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 1);
        let report = job.scan_once().unwrap();
        assert_eq!(report.written.len(), 0);
    }

    #[test]
    fn scan_once_is_idempotent() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "x", &[("README.md", "hello")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);

        let r1 = job.scan_once().unwrap();
        assert_eq!(r1.written.len(), 1);
        assert_eq!(r1.skipped.len(), 0);

        let r2 = job.scan_once().unwrap();
        assert_eq!(r2.written.len(), 0);
        assert_eq!(r2.skipped.len(), 1, "second scan should hash-skip the mirror");
    }

    #[test]
    fn scan_once_rewrites_when_source_changes() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "x", &[("README.md", "v1")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        job.scan_once().unwrap();

        std::fs::write(source.join("x/README.md"), "v2").unwrap();
        let r2 = job.scan_once().unwrap();
        assert_eq!(r2.written.len(), 1, "modified source should re-mirror");
        let body = std::fs::read_to_string(vault.join("projects/x/README.mirror.md")).unwrap();
        assert!(body.contains("v2"));
    }

    #[test]
    fn scan_once_sweeps_stale_mirrors_when_source_removed() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "x", &[("README.md", "y")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        job.scan_once().unwrap();
        assert!(vault.join("projects/x/README.mirror.md").is_file());

        // Remove the source project entirely.
        std::fs::remove_dir_all(source.join("x")).unwrap();
        let report = job.scan_once().unwrap();
        assert_eq!(report.deleted.len(), 1);
        assert!(!vault.join("projects/x/README.mirror.md").is_file());
    }

    #[test]
    fn scan_once_preserves_user_authored_files_in_project_dir() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "x", &[("README.md", "y")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        job.scan_once().unwrap();
        // Plant a user-authored notes.md.
        std::fs::write(vault.join("projects/x/notes.md"), "my private notes").unwrap();

        // Source goes away — mirror is swept, but user file must survive.
        std::fs::remove_dir_all(source.join("x")).unwrap();
        job.scan_once().unwrap();
        assert!(
            vault.join("projects/x/notes.md").is_file(),
            "user file must be preserved through sweep"
        );
    }

    #[test]
    fn scan_once_does_not_touch_mirror_lookalikes_without_generated_header() {
        let (_g, vault, source) = make_dirs();
        // Plant a `.mirror.md` file that wasn't generated by us.
        let foreign = vault.join("projects/handcrafted/draft.mirror.md");
        std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        std::fs::write(&foreign, "user-authored, looks like a mirror but isn't").unwrap();

        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        let report = job.scan_once().unwrap();
        assert_eq!(report.deleted.len(), 0);
        assert!(foreign.is_file(), "foreign mirror lookalike must be preserved");
    }

    #[test]
    fn scan_once_errors_when_source_root_missing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        ensure_layout(&vault).unwrap();
        let job = MirrorJob::new(vault, dir.path().join("does-not-exist"), 2);
        let err = job.scan_once().unwrap_err();
        assert!(matches!(err, MirrorError::SourceMissing(_)));
    }

    #[test]
    fn scan_once_errors_when_projects_dir_missing() {
        let (_g, _vault, source) = make_dirs();
        // Hand-roll an "vault" that lacks projects/
        let bare_vault = source.parent().unwrap().join("empty-vault");
        std::fs::create_dir_all(&bare_vault).unwrap();
        let job = MirrorJob::new(bare_vault, source, 2);
        let err = job.scan_once().unwrap_err();
        assert!(matches!(err, MirrorError::VaultProjectsMissing(_)));
    }

    #[test]
    fn copy_file_mirrors_single_source_file() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "myproj", &[("README.md", "project content")]);
        let source_file = source.join("myproj").join("README.md");
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);

        job.copy_file(&source_file).unwrap();

        let dest = vault.join("projects/myproj/README.mirror.md");
        assert!(dest.is_file());
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(content.contains("project content"));
    }

    #[test]
    fn copy_file_treats_vanished_source_as_remove() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "myproj", &[("README.md", "original")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        // First mirror via scan_once so the dest exists.
        job.scan_once().unwrap();
        let dest = vault.join("projects/myproj/README.mirror.md");
        assert!(dest.is_file());

        // Remove source; copy_file should delete the dest.
        std::fs::remove_file(source.join("myproj/README.md")).unwrap();
        job.copy_file(&source.join("myproj/README.md")).unwrap();
        assert!(!dest.is_file());
    }

    #[test]
    fn handle_remove_deletes_existing_mirror() {
        let (_g, vault, source) = make_dirs();
        make_project(&source, "myproj", &[("README.md", "x")]);
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        job.scan_once().unwrap();
        let dest = vault.join("projects/myproj/README.mirror.md");
        assert!(dest.is_file());

        job.handle_remove(&source.join("myproj/README.md")).unwrap();
        assert!(!dest.is_file());
    }

    #[test]
    fn handle_remove_is_noop_when_mirror_absent() {
        let (_g, vault, source) = make_dirs();
        let job = MirrorJob::new(vault.clone(), source.clone(), 2);
        // No mirror exists — should not error.
        job.handle_remove(&source.join("ghost/README.md")).unwrap();
    }
}
