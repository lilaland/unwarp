//! `VaultManager` — singleton model that owns the vault lifecycle.
//!
//! Phase 2 scope: state machine, layout creation/adoption, lock ownership.
//! No filesystem watcher and no UI subscriptions yet — those land with the
//! vault explorer panel and mirror job in subsequent commits.

use std::path::Path;

use thiserror::Error;
use warpui::{Entity, ModelContext, SingletonEntity};

use super::{
    config::{VaultConfig, VaultConfigError},
    layout::{ensure_layout, LayoutError},
    lock::{VaultLock, VaultLockError},
};

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
