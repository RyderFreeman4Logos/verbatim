use std::fs;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::Result;

use super::{bounded_path, validation_error, validation_message};
use crate::ingest_security::InputSnapshotIdentity;

pub(super) struct HeldInputSnapshot {
    file: fs::File,
    pub(super) identity: InputSnapshotIdentity,
}

impl HeldInputSnapshot {
    pub(super) fn new(file: fs::File, canonical_path: PathBuf) -> Result<Self> {
        let identity = snapshot_opened_file(&canonical_path, &file)?;
        Ok(Self { file, identity })
    }

    pub(super) fn parser_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            process::id(),
            self.file.as_raw_fd()
        ))
    }

    pub(super) fn validate_content_identity(&self, expected_hash: &str) -> Result<()> {
        let actual = snapshot_opened_file(&self.identity.path, &self.file)?;
        if actual != self.identity || actual.content_sha256 != expected_hash {
            return Err(validation_message(
                "held relocation target content changed during validation",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_path_binding(&self, path: &Path) -> Result<()> {
        let entry = open_relocation_target(path)?;
        let held_metadata = self.file.metadata().map_err(|error| {
            relocation_target_io_error("inspect held relocation target", path, error)
        })?;
        let entry_metadata = entry.metadata().map_err(|error| {
            relocation_target_io_error("inspect relocation target entry", path, error)
        })?;
        if !held_metadata.is_file()
            || !entry_metadata.is_file()
            || held_metadata.dev() != entry_metadata.dev()
            || held_metadata.ino() != entry_metadata.ino()
        {
            return Err(validation_message(
                "relocation target path no longer identifies the held snapshot",
            ));
        }
        Ok(())
    }
}

pub(super) fn open_relocation_target(path: &Path) -> Result<fs::File> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| relocation_target_io_error("inspect relocation target", path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(validation_message(format!(
            "relocation target must not be a symlink: {}",
            bounded_path(path)
        )));
    }
    if !metadata.is_file() {
        return Err(validation_message(format!(
            "relocation target is not a regular file: {}",
            bounded_path(path)
        )));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| relocation_target_io_error("open relocation target", path, error))?;
    let opened_metadata = file.metadata().map_err(|error| {
        relocation_target_io_error("inspect opened relocation target", path, error)
    })?;
    if !opened_metadata.is_file() {
        return Err(validation_message(format!(
            "relocation target is not a regular file: {}",
            bounded_path(path)
        )));
    }
    Ok(file)
}

fn snapshot_opened_file(path: &Path, file: &fs::File) -> Result<InputSnapshotIdentity> {
    InputSnapshotIdentity::from_opened_file(path, file).map_err(|error| {
        if snapshot_failure_is_validation(&error) {
            validation_error(error)
        } else {
            error
        }
    })
}

pub(super) fn relocation_target_io_error(
    action: &str,
    path: &Path,
    error: std::io::Error,
) -> anyhow::Error {
    let validation = io_failure_is_target_change(&error);
    let error = anyhow::Error::new(error).context(format!("{action}: {}", bounded_path(path)));
    if validation {
        validation_error(error)
    } else {
        error
    }
}

fn snapshot_failure_is_validation(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_none_or(io_failure_is_target_change)
}

fn io_failure_is_target_change(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::NotFound
        || matches!(
            error.raw_os_error(),
            Some(code) if code == libc::ELOOP || code == libc::EISDIR
        )
}
