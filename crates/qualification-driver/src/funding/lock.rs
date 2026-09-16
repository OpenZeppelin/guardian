use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};

/// Serializes treasury use.
///
/// Two guards are needed for different reasons. Balance checks and transfers
/// must not interleave, or both runs proceed on a balance neither will have.
/// Separately, replay protection is enforced per signer, so two runs signing
/// with the same key concurrently are rejected as replays, which is correct
/// server behaviour and must not be read as a product failure.
///
/// This is a single-machine guard. Across CI runners the workflow's own
/// concurrency group is what serializes runs; this catches the local case,
/// including a developer running two profiles at once.
#[derive(Debug)]
pub struct TreasuryLock {
    _file: File,
    path: PathBuf,
}

impl TreasuryLock {
    pub fn acquire(directory: &Path, scope: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let path = directory.join(format!("{scope}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;

        lock_exclusive(&file).map_err(|error| {
            anyhow!(
                "another run already holds the {scope} treasury lock at {}: {error}",
                path.display()
            )
        })?;

        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // Non-blocking: a run that cannot take the lock should say so rather than
    // hang until a CI timeout kills it with no explanation.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_exclusive_while_held() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = TreasuryLock::acquire(dir.path(), "testnet").expect("first acquires");
        let second = TreasuryLock::acquire(dir.path(), "testnet");
        assert!(second.is_err(), "a second holder must be refused");
        drop(first);
    }

    #[test]
    fn a_lock_is_reusable_once_released() {
        let dir = tempfile::tempdir().expect("temp dir");
        {
            let _held = TreasuryLock::acquire(dir.path(), "testnet").expect("acquires");
        }
        TreasuryLock::acquire(dir.path(), "testnet").expect("acquires again after release");
    }

    #[test]
    fn different_scopes_do_not_contend() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _testnet = TreasuryLock::acquire(dir.path(), "testnet").expect("testnet");
        let _devnet = TreasuryLock::acquire(dir.path(), "devnet").expect("devnet is independent");
    }

    #[test]
    fn the_refusal_names_the_lock_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _held = TreasuryLock::acquire(dir.path(), "testnet").expect("acquires");
        let error = TreasuryLock::acquire(dir.path(), "testnet").expect_err("refused");
        assert!(error.to_string().contains("testnet.lock"));
    }
}
