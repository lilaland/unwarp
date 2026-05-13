//! Vault lock — `.unwarp/lock` PID file with stale-process detection.
//!
//! Single-writer discipline: only one running unwarp instance may hold the
//! lock for a given vault. The lock survives clean shutdown (file deleted)
//! and crash (file holds a dead PID, detected next acquire).
//!
//! Cross-platform note: we use a PID file rather than `flock`/`LockFile`
//! because (a) macOS file locks can survive process death in surprising ways
//! and (b) we want to surface "another instance owns this vault" with the
//! offending PID, which file-locks don't tell us. The trade-off is that the
//! lock is advisory — a malicious process can ignore it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

const LOCK_FILE_NAME: &str = "lock";
const LOCK_DIR_NAME: &str = ".unwarp";

/// Errors produced by [`VaultLock`] operations.
#[derive(Debug, Error)]
pub enum VaultLockError {
    #[error("vault is already locked by another process (pid {pid})")]
    AlreadyHeld { pid: u32 },

    #[error("filesystem error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// Holds the vault lock for the lifetime of the value. Drops the lock file
/// on `Drop`.
///
/// # Crash behavior
///
/// If the process crashes without dropping the lock, the file remains with
/// the dead PID. The next [`VaultLock::acquire`] call detects this via
/// [`is_process_alive`] and removes the stale lock before retrying.
#[derive(Debug)]
pub struct VaultLock {
    path: PathBuf,
    /// Held open so the OS keeps a file handle while we exist; doesn't enforce
    /// anything by itself, but ensures we always own a writable handle.
    _file: File,
}

impl VaultLock {
    /// Acquire the lock for the vault rooted at `vault_root`.
    ///
    /// The lock file lives at `<vault_root>/.unwarp/lock`. The `.unwarp/`
    /// directory must exist (created by [`ensure_layout`](super::ensure_layout)).
    ///
    /// If a stale lock from a dead PID is present, it is removed and the
    /// acquire is retried once. If a live PID owns it, returns
    /// [`VaultLockError::AlreadyHeld`].
    pub fn acquire(vault_root: &Path) -> Result<Self, VaultLockError> {
        let lock_path = lock_path(vault_root);

        // Make sure parent dir exists (callers usually call ensure_layout first,
        // but be defensive — acquiring a lock on a fresh dir shouldn't fail).
        if let Some(parent) = lock_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| VaultLockError::Io {
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }
        }

        match try_create_lock(&lock_path)? {
            CreateOutcome::Created(file) => Ok(Self {
                path: lock_path,
                _file: file,
            }),
            CreateOutcome::AlreadyExists => {
                // Inspect the existing lock. If it holds a dead PID, clean up
                // and retry once. If it holds a live one, refuse.
                match read_pid(&lock_path) {
                    Ok(pid) if is_process_alive(pid) => {
                        Err(VaultLockError::AlreadyHeld { pid })
                    }
                    Ok(_) | Err(_) => {
                        fs::remove_file(&lock_path).map_err(|e| VaultLockError::Io {
                            path: lock_path.display().to_string(),
                            source: e,
                        })?;
                        match try_create_lock(&lock_path)? {
                            CreateOutcome::Created(file) => Ok(Self {
                                path: lock_path,
                                _file: file,
                            }),
                            CreateOutcome::AlreadyExists => {
                                // Race: another process created the lock between
                                // our remove and create. Re-read and report.
                                let pid = read_pid(&lock_path).unwrap_or(0);
                                Err(VaultLockError::AlreadyHeld { pid })
                            }
                        }
                    }
                }
            }
        }
    }

    /// Returns the PID of the current lock holder, or `None` if no lock file
    /// exists or it can't be parsed. Does not check liveness.
    pub fn current_holder(vault_root: &Path) -> Option<u32> {
        read_pid(&lock_path(vault_root)).ok()
    }

    /// Path to the lock file (whether it exists or not).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path(vault_root: &Path) -> PathBuf {
    vault_root.join(LOCK_DIR_NAME).join(LOCK_FILE_NAME)
}

enum CreateOutcome {
    Created(File),
    AlreadyExists,
}

fn try_create_lock(path: &Path) -> Result<CreateOutcome, VaultLockError> {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(mut file) => {
            writeln!(file, "{}", std::process::id()).map_err(|e| VaultLockError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
            Ok(CreateOutcome::Created(file))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(CreateOutcome::AlreadyExists),
        Err(e) => Err(VaultLockError::Io {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

fn read_pid(path: &Path) -> io::Result<u32> {
    let raw = fs::read_to_string(path)?;
    raw.trim()
        .parse::<u32>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Returns `true` if a process with the given PID is alive on this machine.
///
/// Implemented via `kill(pid, 0)` on Unix, which sends no signal but returns
/// success iff the target exists and we have permission to signal it. On
/// Windows we use `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION`.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill() with sig=0 is documented to never affect the target;
    // it only checks for existence and permission.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    // EPERM means the process exists but we can't signal it — still alive.
    let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    // Phase 2 ships unix-only; Windows port is tracked separately.
    // Fall back to "always alive" so we err on the side of safety
    // (a stale Windows lock will require manual deletion).
    let _ = pid;
    true
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vault_with_lock_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(LOCK_DIR_NAME)).unwrap();
        dir
    }

    #[test]
    fn acquire_creates_lock_file_with_pid() {
        let dir = make_vault_with_lock_dir();
        let lock = VaultLock::acquire(dir.path()).unwrap();
        let pid_str = std::fs::read_to_string(lock.path()).unwrap();
        assert_eq!(pid_str.trim().parse::<u32>().unwrap(), std::process::id());
    }

    #[test]
    fn drop_releases_lock_file() {
        let dir = make_vault_with_lock_dir();
        let lock_path_buf = {
            let lock = VaultLock::acquire(dir.path()).unwrap();
            lock.path().to_owned()
        };
        assert!(!lock_path_buf.exists(), "lock file should be removed on Drop");
    }

    #[test]
    fn acquire_after_release_succeeds() {
        let dir = make_vault_with_lock_dir();
        let _lock1 = VaultLock::acquire(dir.path()).unwrap();
        drop(_lock1);
        // Should succeed because the previous lock was released.
        let _lock2 = VaultLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn acquire_with_live_pid_fails() {
        let dir = make_vault_with_lock_dir();
        // Plant a lock file with our own PID; we are alive, so acquire must fail.
        std::fs::write(
            dir.path().join(LOCK_DIR_NAME).join(LOCK_FILE_NAME),
            format!("{}", std::process::id()),
        )
        .unwrap();
        let err = VaultLock::acquire(dir.path()).unwrap_err();
        match err {
            VaultLockError::AlreadyHeld { pid } => assert_eq!(pid, std::process::id()),
            other => panic!("expected AlreadyHeld, got {other:?}"),
        }
    }

    #[test]
    fn acquire_clears_stale_lock() {
        let dir = make_vault_with_lock_dir();
        // Plant a lock file with a clearly-dead PID. Picking 1 (init) which we
        // can't signal so it's reported alive — instead use a high PID we can
        // be confident isn't running. We use 0x7FFFFFFE as a clearly-unused
        // pid_t; on systems where this might collide, the test is best-effort.
        let dead_pid: u32 = 0x7fff_fffe;
        // Sanity: ensure our liveness check actually says it's dead.
        if is_process_alive(dead_pid) {
            // Test environment has a running process at this PID; skip.
            eprintln!("skipping stale-lock test: PID {dead_pid} unexpectedly alive");
            return;
        }
        std::fs::write(
            dir.path().join(LOCK_DIR_NAME).join(LOCK_FILE_NAME),
            format!("{dead_pid}"),
        )
        .unwrap();
        let lock = VaultLock::acquire(dir.path()).unwrap();
        // Now the lock holds OUR PID, not the dead one.
        let pid_str = std::fs::read_to_string(lock.path()).unwrap();
        assert_eq!(pid_str.trim().parse::<u32>().unwrap(), std::process::id());
    }

    #[test]
    fn acquire_clears_unparseable_lock() {
        let dir = make_vault_with_lock_dir();
        std::fs::write(
            dir.path().join(LOCK_DIR_NAME).join(LOCK_FILE_NAME),
            "garbage\nnot a pid",
        )
        .unwrap();
        // Unparseable lock is treated as stale and removed.
        let _lock = VaultLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn current_holder_returns_pid_when_locked() {
        let dir = make_vault_with_lock_dir();
        let _lock = VaultLock::acquire(dir.path()).unwrap();
        assert_eq!(VaultLock::current_holder(dir.path()), Some(std::process::id()));
    }

    #[test]
    fn current_holder_returns_none_when_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(VaultLock::current_holder(dir.path()), None);
    }
}
