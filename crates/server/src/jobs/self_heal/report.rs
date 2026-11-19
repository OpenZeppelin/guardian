//! The recovery file: one entry for each account that the block could not
//! repair.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::types::{SelfHealTotals, UnrecoveredAccount};

/// The schema version of the recovery file.
pub const RECOVERY_REPORT_SCHEMA: u32 = 1;

/// The content of the recovery file.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryReport {
    pub schema: u32,
    pub generated_at: String,
    pub network: String,
    pub dry_run: bool,
    pub totals: SelfHealTotals,
    pub accounts: Vec<UnrecoveredAccount>,
}

impl RecoveryReport {
    /// A report of the current schema. The accounts are sorted by id.
    pub fn new(
        generated_at: String,
        network: String,
        dry_run: bool,
        totals: SelfHealTotals,
        mut accounts: Vec<UnrecoveredAccount>,
    ) -> Self {
        accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        Self {
            schema: RECOVERY_REPORT_SCHEMA,
            generated_at,
            network,
            dry_run,
            totals,
            accounts,
        }
    }

    /// Write the report as pretty JSON. The write goes to a temporary file
    /// in the same directory, which is then renamed over `path`, so a
    /// reader sees the old file or the new file, never a part. On unix the
    /// file is readable and writable by its owner only.
    pub fn write_atomic(&self, path: &Path) -> Result<(), String> {
        let mut body = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("failed to encode the recovery file: {error}"))?;
        body.push(b'\n');
        let temp = temp_path(path)?;
        let written = write_owner_only(&temp, &body).and_then(|()| rename_into_place(&temp, path));
        if written.is_err() {
            remove_if_present(&temp)?;
        }
        written?;
        sync_directory(&parent_directory(path));
        Ok(())
    }
}

fn parent_directory(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn temp_path(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("recovery file path {} has no file name", path.display()))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.tmp", std::process::id()));
    Ok(parent_directory(path).join(temp_name))
}

fn write_owner_only(path: &Path, body: &[u8]) -> Result<(), String> {
    remove_if_present(path)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn rename_into_place(temp: &Path, path: &Path) -> Result<(), String> {
    fs::rename(temp, path).map_err(|error| {
        format!(
            "failed to move {} to {}: {error}",
            temp.display(),
            path.display()
        )
    })
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to remove {}: {error}", path.display())),
    }
}

/// Make the rename durable. Some file systems refuse to sync a directory;
/// the file itself is already synced, so a failure here only warns.
#[cfg(unix)]
fn sync_directory(directory: &Path) {
    match fs::File::open(directory).and_then(|handle| handle.sync_all()) {
        Ok(()) => {}
        Err(error) => tracing::warn!(
            directory = %directory.display(),
            error = %error,
            "could not sync the directory of the recovery file"
        ),
    }
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) {}
