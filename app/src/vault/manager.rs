//! `VaultManager` — singleton model that owns the vault lifecycle, including
//! the notify-based filesystem watcher for vault and mirror-source trees.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use warpui::{Entity, ModelContext, ModelHandle, SingletonEntity};

#[cfg(not(target_family = "wasm"))]
use notify_debouncer_full::notify::{RecursiveMode, WatchFilter};
#[cfg(not(target_family = "wasm"))]
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

use super::{
    config::{VaultConfig, VaultConfigError},
    layout::{ensure_layout, LayoutError},
    lock::{VaultLock, VaultLockError},
    mirror::job::MirrorJob,
};

const MIRROR_SOURCE_FILES: &[&str] = &["README.md", "AGENTS.md", "CLAUDE.md"];
const WATCHER_DEBOUNCE_MS: u64 = 500;

/// State of the vault as known to the app.
///
/// State transitions:
/// - `Uninitialized` → `Ready` via `initialize` / `create_new` / `adopt_existing`
/// - `Ready` → `Error` if the vault directory disappears mid-session (detected
///   on next operation; not background-polled in Phase 2 foundation)
/// - `Error` / `Locked` → `Ready` via re-initialization
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultState {
    /// No vault configured / no path attempted yet. Workspace shows the
    /// first-run banner.
    Uninitialized,

    /// Vault is set up, locked by us, and ready for reads/writes.
    Ready,

    /// Vault is owned by a different unwarp instance. Includes the holder PID
    /// for the user-facing message.
    Locked { by_pid: u32 },

    /// Something failed: directory missing, layout creation failed, etc.
    /// Workspace renders the message as a banner.
    Error(String),
}

/// Events emitted by the manager.
#[derive(Debug, Clone)]
pub enum VaultManagerEvent {
    /// State changed — `new` is the post-transition state.
    StateChanged { new: VaultState },

    /// A file in the vault root was created, modified, or removed.
    /// Subscribers (e.g. VaultPanel) should refresh their tree view.
    /// `path` is the absolute path of the changed file.
    FileChanged { path: PathBuf },
}

/// Errors surfaced by manager operations. These are also folded into
/// `VaultState::Error(String)` for display.
#[derive(Debug, Error)]
pub enum VaultManagerError {
    #[error("invalid vault config: {0}")]
    Config(#[from] VaultConfigError),

    #[error("could not set up vault layout: {0}")]
    Layout(#[from] LayoutError),

    #[error("could not lock vault: {0}")]
    Lock(#[from] VaultLockError),
}

/// The vault manager singleton. Lives in `AppContext` via
/// `ctx.add_singleton_model(VaultManager::new)`.
pub struct VaultManager {
    config: Option<VaultConfig>,
    lock: Option<VaultLock>,
    state: VaultState,
    /// Sub-model that owns the notify-based filesystem watcher.
    /// Replaced (old watcher silently abandoned) whenever the vault is
    /// re-initialized to a new path.
    #[cfg(not(target_family = "wasm"))]
    _fs_watcher: Option<ModelHandle<BulkFilesystemWatcher>>,
}

impl VaultManager {
    /// Construct an uninitialized manager. Called by
    /// `ctx.add_singleton_model(VaultManager::new)` at app startup; the
    /// workspace layer (or first-run banner) drives `initialize` afterwards.
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            config: None,
            lock: None,
            state: VaultState::Uninitialized,
            #[cfg(not(target_family = "wasm"))]
            _fs_watcher: None,
        }
    }

    /// Adopt or create the vault at `config.root` based on whether it already
    /// looks like a vault. Convenience entry point for "load on app startup
    /// from settings" flows.
    pub fn initialize(
        &mut self,
        config: VaultConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), VaultManagerError> {
        if VaultConfig::is_recognized_vault(&config.root) {
            self.adopt_existing(config, ctx)
        } else {
            self.create_new(config, ctx)
        }
    }

    /// Create a brand-new vault at `config.root`. Idempotent — safe to call
    /// against an existing path; `ensure_layout` only writes what's missing.
    pub fn create_new(
        &mut self,
        config: VaultConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), VaultManagerError> {
        ensure_layout(&config.root)?;
        self.lock_and_set_ready(config, ctx)
    }

    /// Adopt an existing vault. Validates the layout (creating only the
    /// unwarp-managed subdirs that are missing); never touches `notes/`.
    pub fn adopt_existing(
        &mut self,
        config: VaultConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), VaultManagerError> {
        ensure_layout(&config.root)?;
        self.lock_and_set_ready(config, ctx)
    }

    fn lock_and_set_ready(
        &mut self,
        config: VaultConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), VaultManagerError> {
        // Drop the old lock first (if any) to avoid leaking a stale handle
        // when the user re-points the vault.
        self.lock = None;

        match VaultLock::acquire(&config.root) {
            Ok(lock) => {
                self.lock = Some(lock);
                #[cfg(not(target_family = "wasm"))]
                self.start_watcher(&config, ctx);
                self.config = Some(config);
                self.set_state(VaultState::Ready, ctx);
                Ok(())
            }
            Err(VaultLockError::AlreadyHeld { pid }) => {
                self.config = Some(config);
                self.set_state(VaultState::Locked { by_pid: pid }, ctx);
                Err(VaultManagerError::Lock(VaultLockError::AlreadyHeld { pid }))
            }
            Err(other) => {
                let msg = other.to_string();
                self.set_state(VaultState::Error(msg), ctx);
                Err(VaultManagerError::Lock(other))
            }
        }
    }

    /// Create a new `BulkFilesystemWatcher` sub-model and register:
    /// - vault root (recursive, all files) → emits `FileChanged` on any event
    /// - mirror source root (recursive, filtered to mirror filenames) →
    ///   also runs `MirrorJob::copy_file` before emitting `FileChanged`
    #[cfg(not(target_family = "wasm"))]
    fn start_watcher(&mut self, config: &VaultConfig, ctx: &mut ModelContext<Self>) {
        let watcher = ctx.add_model(|ctx| {
            BulkFilesystemWatcher::new(Duration::from_millis(WATCHER_DEBOUNCE_MS), ctx)
        });

        let vault_root = config.root.clone();
        let source_root = config.mirror_source_root.clone();

        // Watch vault root — all events trigger a tree refresh.
        let vault_root_clone = vault_root.clone();
        let reg_vault = watcher.update(ctx, move |w, _| {
            w.register_path(&vault_root_clone, WatchFilter::accept_all(), RecursiveMode::Recursive)
        });
        ctx.spawn(reg_vault, |_, result, _| {
            if let Err(e) = result {
                log::warn!("vault watcher: failed to watch vault root: {e}");
            }
        });

        // Watch source root — filtered to mirror-relevant filenames only.
        let mirror_names: Arc<[&'static str]> = Arc::from(MIRROR_SOURCE_FILES);
        let reg_source = watcher.update(ctx, move |w, _| {
            let names = mirror_names.clone();
            w.register_path(
                &source_root,
                WatchFilter::with_filter(Arc::new(move |path: &Path| {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| names.contains(&n))
                })),
                RecursiveMode::Recursive,
            )
        });
        ctx.spawn(reg_source, |_, result, _| {
            if let Err(e) = result {
                log::warn!("vault watcher: failed to watch mirror source root: {e}");
            }
        });

        ctx.subscribe_to_model(&watcher, move |me, event: &BulkFilesystemWatcherEvent, ctx| {
            me.handle_fs_event(event, &vault_root, ctx);
        });

        self._fs_watcher = Some(watcher);
    }

    /// Dispatch filesystem events from the sub-model watcher.
    ///
    /// - Events under `vault_root` → emit `FileChanged` so the explorer tree refreshes.
    /// - Events for mirror source files (README/AGENTS/CLAUDE) → copy into vault
    ///   via `MirrorJob`, then emit `FileChanged` for the updated vault path.
    #[cfg(not(target_family = "wasm"))]
    fn handle_fs_event(
        &mut self,
        event: &BulkFilesystemWatcherEvent,
        vault_root: &Path,
        ctx: &mut ModelContext<Self>,
    ) {
        let all_paths: Vec<PathBuf> = event
            .added
            .iter()
            .chain(event.modified.iter())
            .chain(event.deleted.iter())
            .chain(event.moved.keys())
            .chain(event.moved.values())
            .cloned()
            .collect();

        let Some(config) = self.config.as_ref() else {
            return;
        };

        for path in all_paths {
            if path.starts_with(vault_root) {
                ctx.emit(VaultManagerEvent::FileChanged { path });
            } else {
                // Source-root file — trigger a mirror copy if it's a mirror file.
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !MIRROR_SOURCE_FILES.contains(&file_name) {
                    continue;
                }
                // Deleted source file → remove mirror.
                let is_deleted = event.deleted.contains(&path);
                let mirror_job = MirrorJob::new(
                    config.root.clone(),
                    config.mirror_source_root.clone(),
                    config.mirror_max_depth,
                );
                let vault_root_owned = vault_root.to_path_buf();
                let path_clone = path.clone();
                ctx.spawn(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            if is_deleted {
                                let _ = mirror_job.handle_remove(&path_clone);
                            } else {
                                let _ = mirror_job.copy_file(&path_clone);
                            }
                            vault_root_owned
                        })
                        .await
                        .unwrap_or_else(|_| PathBuf::new())
                    },
                    |me, vault_root_owned, ctx| {
                        if !vault_root_owned.as_os_str().is_empty() {
                            ctx.emit(VaultManagerEvent::FileChanged {
                                path: vault_root_owned,
                            });
                        }
                        let _ = me; // suppress unused warning
                    },
                );
            }
        }
    }

    fn set_state(&mut self, new: VaultState, ctx: &mut ModelContext<Self>) {
        if self.state == new {
            return;
        }
        self.state = new.clone();
        ctx.emit(VaultManagerEvent::StateChanged { new });
        ctx.notify();
    }

    /// Returns the absolute vault root, if a config has been applied (even on
    /// `Locked` / `Error` states).
    pub fn vault_root(&self) -> Option<&Path> {
        self.config.as_ref().map(|c| c.root.as_path())
    }

    pub fn config(&self) -> Option<&VaultConfig> {
        self.config.as_ref()
    }

    pub fn state(&self) -> &VaultState {
        &self.state
    }

    /// True if the vault is in a usable read/write state.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, VaultState::Ready)
    }
}

impl Entity for VaultManager {
    type Event = VaultManagerEvent;
}

impl SingletonEntity for VaultManager {}

// VaultManager unit tests would need a real warpui AppContext via App::test
// (asset provider, foreground executor, async future). The state-transition
// logic itself is composed of layout::ensure_layout, lock::VaultLock, and
// VaultConfig — all of which have direct unit tests in their respective
// modules. End-to-end manager behavior is exercised once we wire the
// manager into the workspace's first-run flow (next commit in Phase 2).
