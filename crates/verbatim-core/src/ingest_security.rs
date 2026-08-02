//! Shared fail-closed policy and helpers for ingest untrusted-execution boundaries.
//!
//! INGEST-SEC-001 / issue #337: every parser, converter, archive tool, OCR adapter,
//! and filesystem path accepted at ingest is treated as an untrusted execution
//! boundary. This module is the first walking skeleton: policy defaults, path
//! containment, archive counters, and immutable input snapshot identity.
//!
//! Residual (not in this slice): full OS sandbox (bubblewrap/landlock/seccomp),
//! complete archive extractor rewrite, fuzz harness expansion, and wiring every
//! ingest entry point. See `docs/architecture/ingest-security-boundary.md`.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::image_limits::ImageArtifactLimits;
use crate::types::hex_sha256;

const INPUT_SNAPSHOT_HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Fail-closed defaults for untrusted ingest helpers and external tools.
///
/// Network access is denied by default (`allow_network == false`). Image
/// dimension/pixel bounds are owned by [`ImageArtifactLimits`] and composed
/// here rather than duplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestSecurityPolicy {
    /// When false (default), external converters/OCR must not be configured for
    /// network use and proxy-related environment variables are stripped.
    #[serde(default = "default_allow_network")]
    pub allow_network: bool,
    /// Maximum total expanded bytes admitted from a single archive tree.
    #[serde(default = "default_max_expanded_archive_bytes")]
    pub max_expanded_archive_bytes: u64,
    /// Maximum number of archive members admitted from a single archive.
    #[serde(default = "default_max_archive_members")]
    pub max_archive_members: usize,
    /// Maximum nesting depth of nested archives.
    #[serde(default = "default_max_archive_nesting_depth")]
    pub max_archive_nesting_depth: usize,
    /// Maximum wall-clock time for one untrusted helper invocation.
    #[serde(default = "default_max_wall_time_seconds")]
    pub max_wall_time_seconds: u64,
    /// Maximum stdout bytes retained from an external helper.
    #[serde(default = "default_max_stdout_bytes")]
    pub max_stdout_bytes: usize,
    /// Maximum stderr bytes retained from an external helper.
    #[serde(default = "default_max_stderr_bytes")]
    pub max_stderr_bytes: usize,
    /// When true (default), failed untrusted extraction/conversion results are
    /// quarantined rather than partially applied.
    #[serde(default = "default_quarantine_on_failure")]
    pub quarantine_on_failure: bool,
    /// Image artifact bounds (owned by `image_limits`; composed, not copied).
    #[serde(default)]
    pub image_limits: ImageArtifactLimits,
}

impl Default for IngestSecurityPolicy {
    fn default() -> Self {
        Self {
            allow_network: default_allow_network(),
            max_expanded_archive_bytes: default_max_expanded_archive_bytes(),
            max_archive_members: default_max_archive_members(),
            max_archive_nesting_depth: default_max_archive_nesting_depth(),
            max_wall_time_seconds: default_max_wall_time_seconds(),
            max_stdout_bytes: default_max_stdout_bytes(),
            max_stderr_bytes: default_max_stderr_bytes(),
            quarantine_on_failure: default_quarantine_on_failure(),
            image_limits: ImageArtifactLimits::default(),
        }
    }
}

impl IngestSecurityPolicy {
    /// Wall-clock budget as a [`Duration`].
    pub fn max_wall_time(&self) -> Duration {
        Duration::from_secs(self.max_wall_time_seconds.max(1))
    }

    /// Apply fail-closed environment hardening to an external command spawn.
    ///
    /// This is not a full OS sandbox. When `allow_network` is false (default),
    /// proxy and common network-client environment variables are removed so the
    /// child does not inherit ambient network configuration. Callers still own
    /// process-group isolation and I/O caps.
    pub fn apply_to_external_command(&self, command: &mut std::process::Command) {
        if !self.allow_network {
            for key in NETWORK_ENV_KEYS {
                command.env_remove(key);
            }
        }
    }

    /// Reject archive member counts that exceed the policy bound.
    pub fn check_archive_member_count(&self, count: usize) -> Result<()> {
        check_archive_member_count(count, self.max_archive_members)
    }

    /// Reject archive nesting depths that exceed the policy bound.
    pub fn check_archive_nesting_depth(&self, depth: usize) -> Result<()> {
        check_archive_nesting_depth(depth, self.max_archive_nesting_depth)
    }

    /// Reject expanded archive byte totals that exceed the policy bound.
    pub fn check_expanded_bytes(&self, expanded_bytes: u64) -> Result<()> {
        check_expanded_bytes(expanded_bytes, self.max_expanded_archive_bytes)
    }
}

fn default_allow_network() -> bool {
    false
}

fn default_max_expanded_archive_bytes() -> u64 {
    512 * 1024 * 1024
}

fn default_max_archive_members() -> usize {
    10_000
}

fn default_max_archive_nesting_depth() -> usize {
    3
}

fn default_max_wall_time_seconds() -> u64 {
    120
}

fn default_max_stdout_bytes() -> usize {
    4 * 1024 * 1024
}

fn default_max_stderr_bytes() -> usize {
    64 * 1024
}

fn default_quarantine_on_failure() -> bool {
    true
}

/// Environment keys that can enable or reconfigure outbound network clients.
const NETWORK_ENV_KEYS: &[&str] = &[
    "ALL_PROXY",
    "all_proxy",
    "FTP_PROXY",
    "ftp_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "SOCKS_PROXY",
    "socks_proxy",
    "SSLKEYLOGFILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

/// Pure archive-member bound check (shared by extractors and tests).
pub fn check_archive_member_count(count: usize, limit: usize) -> Result<()> {
    if count > limit {
        bail!("archive member count {count} exceeds limit {limit}");
    }
    Ok(())
}

/// Pure archive-nesting bound check (shared by extractors and tests).
pub fn check_archive_nesting_depth(depth: usize, limit: usize) -> Result<()> {
    if depth > limit {
        bail!("archive nesting depth {depth} exceeds limit {limit}");
    }
    Ok(())
}

/// Pure expanded-bytes bound check (shared by extractors and tests).
pub fn check_expanded_bytes(expanded_bytes: u64, limit: u64) -> Result<()> {
    if expanded_bytes > limit {
        bail!("expanded archive bytes {expanded_bytes} exceed limit {limit}");
    }
    Ok(())
}

/// Immutable identity for an ingest input snapshot.
///
/// Small fixtures hash full file bytes. Large-file call sites may populate
/// size/mtime/path first and stream a hash separately; this constructor always
/// hashes the current file contents for deterministic tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSnapshotIdentity {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
}

impl InputSnapshotIdentity {
    /// Build an identity from one opened file plus a full content digest.
    pub fn from_path(path: &Path) -> Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .with_context(|| format!("open input snapshot {}", path.display()))?;
        Self::from_opened_file(path, &file)
    }

    /// Hash one already-opened file without resolving its path again.
    pub(crate) fn from_opened_file(path: &Path, file: &fs::File) -> Result<Self> {
        let before = file
            .metadata()
            .with_context(|| format!("stat opened input snapshot {}", path.display()))?;
        if !before.is_file() {
            bail!("input snapshot is not a regular file: {}", path.display());
        }
        let mut reader = file
            .try_clone()
            .with_context(|| format!("clone opened input snapshot {}", path.display()))?;
        reader
            .seek(SeekFrom::Start(0))
            .with_context(|| format!("rewind opened input snapshot {}", path.display()))?;
        let content_sha256 = sha256_reader(&mut reader)
            .with_context(|| format!("read input snapshot {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("restat opened input snapshot {}", path.display()))?;
        if before.len() != metadata.len()
            || before.modified().ok() != metadata.modified().ok()
            || file_inode(&before) != file_inode(&metadata)
        {
            bail!("input snapshot changed while reading: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            size_bytes: metadata.len(),
            content_sha256,
            modified: metadata.modified().ok(),
            inode: file_inode(&metadata),
        })
    }

    /// Build an identity from already-buffered content (tests / small fixtures).
    pub fn from_bytes(path: impl Into<PathBuf>, bytes: &[u8]) -> Self {
        Self {
            path: path.into(),
            size_bytes: bytes.len() as u64,
            content_sha256: hex_sha256(bytes),
            modified: None,
            inode: None,
        }
    }
}

fn sha256_reader(reader: &mut impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; INPUT_SNAPSHOT_HASH_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn file_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn file_inode(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

/// Join `relative` under `root`, rejecting zip-slip / absolute escape paths.
///
/// Components are validated before joining. The result is always a path that
/// lexically stays under `root` (no `..`, no absolute segments, no empty or
/// device-ish names). Symlink resolution of the final path is handled by
/// [`validate_contained_path`].
pub fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative_components(relative)?;
    let mut joined = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            _ => bail!("unsafe path component in {}", relative.display()),
        }
    }
    // Lexical containment: strip_prefix after cleaning root + relative.
    let root_clean = normalize_lexical(root);
    let joined_clean = normalize_lexical(&joined);
    if !joined_clean.starts_with(&root_clean) {
        bail!(
            "path escapes root {}: {}",
            root.display(),
            relative.display()
        );
    }
    Ok(joined)
}

/// Validate that `candidate` is contained under `root`.
///
/// Rejects absolute candidates that are outside `root`, relative zip-slip
/// candidates, and — when the candidate already exists — resolved symlink
/// targets that leave `root`. Pure relative candidates that do not yet exist
/// are checked lexically via [`safe_join`].
pub fn validate_contained_path(root: &Path, candidate: &Path) -> Result<()> {
    if candidate.as_os_str().is_empty() {
        bail!("empty path is not allowed under {}", root.display());
    }

    // Reject zip-slip / absolute relative components before requiring the root
    // to exist on disk (fail closed on path shape alone).
    if !candidate.is_absolute() {
        validate_relative_components(candidate)?;
    }

    let root_abs = canonicalize_existing(root)
        .with_context(|| format!("resolve containment root {}", root.display()))?;

    if candidate.is_absolute() {
        let candidate_abs = if candidate.exists() {
            canonicalize_existing(candidate)
                .with_context(|| format!("resolve candidate {}", candidate.display()))?
        } else {
            normalize_lexical(candidate)
        };
        if !path_is_under(&candidate_abs, &root_abs) {
            bail!(
                "absolute path escapes root {}: {}",
                root.display(),
                candidate.display()
            );
        }
        return Ok(());
    }

    let joined = safe_join(&root_abs, candidate)?;
    if joined.exists() {
        // Resolve final path without following intermediate escape links more
        // than canonicalize allows; reject if the resolved target leaves root.
        let resolved = canonicalize_existing(&joined)
            .with_context(|| format!("resolve contained candidate {}", joined.display()))?;
        if !path_is_under(&resolved, &root_abs) {
            bail!(
                "resolved path escapes root {}: {}",
                root.display(),
                candidate.display()
            );
        }
    }
    Ok(())
}

fn validate_relative_components(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty() {
        bail!("empty relative path is not allowed");
    }
    if relative.is_absolute() {
        bail!(
            "absolute path is not allowed as relative join: {}",
            relative.display()
        );
    }
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.is_empty() {
                    bail!("empty path component is not allowed");
                }
                if is_device_ish_name(&name) {
                    bail!("device-ish path component is not allowed: {name}");
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                bail!(
                    "parent path component '..' is not allowed: {}",
                    relative.display()
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "absolute path component is not allowed: {}",
                    relative.display()
                );
            }
        }
    }
    Ok(())
}

fn is_device_ish_name(name: &str) -> bool {
    // Unix null byte already cannot appear in OsStr components from Path, but
    // reject reserved Windows device names when they appear as a component.
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "nul"
            | "con"
            | "prn"
            | "aux"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "lpt1"
            | "lpt2"
            | "lpt3"
    )
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn policy_defaults_deny_network_and_quarantine() {
        let policy = IngestSecurityPolicy::default();
        assert!(!policy.allow_network);
        assert!(policy.quarantine_on_failure);
        assert!(policy.max_expanded_archive_bytes > 0);
        assert!(policy.max_archive_members > 0);
        assert!(policy.max_archive_nesting_depth > 0);
        assert_eq!(policy.image_limits, ImageArtifactLimits::default());
    }

    #[test]
    fn safe_join_accepts_relative_subdir() {
        let root = Path::new("/tmp/ingest-root");
        let joined = safe_join(root, Path::new("subdir/file.txt")).unwrap();
        assert_eq!(joined, PathBuf::from("/tmp/ingest-root/subdir/file.txt"));
    }

    #[test]
    fn zip_slip_parent_escape_is_rejected() {
        let root = Path::new("/tmp/ingest-root");
        let err = safe_join(root, Path::new("../escape/file"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("..") || err.contains("parent"), "{err}");
        let err = validate_contained_path(root, Path::new("../escape/file"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("..") || err.contains("parent") || err.contains("escape"),
            "{err}"
        );
    }

    #[test]
    fn absolute_path_join_is_rejected() {
        let root = Path::new("/tmp/ingest-root");
        let err = safe_join(root, Path::new("/etc/passwd"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn nested_parent_segments_are_rejected() {
        let root = Path::new("/tmp/ingest-root");
        let err = safe_join(root, Path::new("a/../../etc/passwd"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("..") || err.contains("parent"), "{err}");
    }

    #[test]
    fn safe_relative_path_stays_under_root() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file.txt"), b"ok").unwrap();
        let joined = safe_join(root, Path::new("subdir/file.txt")).unwrap();
        assert!(joined.starts_with(root));
        validate_contained_path(root, Path::new("subdir/file.txt")).unwrap();
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, b"secret").unwrap();
        let link = root.join("escape-link");
        symlink(&outside, &link).unwrap();

        let err = validate_contained_path(&root, Path::new("escape-link"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("escape") || err.contains("resolve") || err.contains("canonicalize"),
            "{err}"
        );
    }

    #[test]
    fn archive_bound_checks_reject_over_limit() {
        check_archive_member_count(10, 10).unwrap();
        check_archive_member_count(11, 10).unwrap_err();
        check_archive_nesting_depth(3, 3).unwrap();
        check_archive_nesting_depth(4, 3).unwrap_err();
        check_expanded_bytes(100, 100).unwrap();
        check_expanded_bytes(101, 100).unwrap_err();

        let policy = IngestSecurityPolicy {
            max_archive_members: 2,
            max_archive_nesting_depth: 1,
            max_expanded_archive_bytes: 50,
            ..IngestSecurityPolicy::default()
        };
        policy.check_archive_member_count(3).unwrap_err();
        policy.check_archive_nesting_depth(2).unwrap_err();
        policy.check_expanded_bytes(51).unwrap_err();
    }

    #[test]
    fn input_snapshot_identity_differs_when_content_changes() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("fixture.txt");
        fs::write(&path, b"alpha").unwrap();
        let first = InputSnapshotIdentity::from_path(&path).unwrap();
        fs::write(&path, b"beta").unwrap();
        let second = InputSnapshotIdentity::from_path(&path).unwrap();
        assert_ne!(first.content_sha256, second.content_sha256);
        assert_eq!(first.size_bytes, 5);
        assert_eq!(second.size_bytes, 4);

        let from_bytes = InputSnapshotIdentity::from_bytes(&path, b"beta");
        assert_eq!(from_bytes.content_sha256, second.content_sha256);
    }

    #[test]
    fn input_snapshot_hashing_streams_large_inputs_with_a_fixed_buffer() {
        const INPUT_BYTES: usize = 8 * 1024 * 1024;

        struct BoundedZeroReader {
            remaining: usize,
        }

        impl Read for BoundedZeroReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert!(buffer.len() <= INPUT_SNAPSHOT_HASH_BUFFER_BYTES);
                let read = self.remaining.min(buffer.len());
                buffer[..read].fill(0);
                self.remaining -= read;
                Ok(read)
            }
        }

        let actual = sha256_reader(&mut BoundedZeroReader {
            remaining: INPUT_BYTES,
        })
        .unwrap();
        let mut expected = Sha256::new();
        let block = [0_u8; INPUT_SNAPSHOT_HASH_BUFFER_BYTES];
        for _ in 0..(INPUT_BYTES / block.len()) {
            expected.update(block);
        }
        assert_eq!(actual, format!("{:x}", expected.finalize()));
    }

    #[test]
    fn apply_to_external_command_strips_proxy_env_when_network_denied() {
        let policy = IngestSecurityPolicy::default();
        assert!(!policy.allow_network);
        let mut command = Command::new("true");
        command.env("HTTP_PROXY", "http://evil.example");
        command.env("https_proxy", "http://evil.example");
        command.env("PATH", "/usr/bin");
        policy.apply_to_external_command(&mut command);
        let envs: Vec<(String, String)> = command
            .get_envs()
            .filter_map(|(k, v)| {
                let key = k.to_string_lossy().into_owned();
                let value = v?.to_string_lossy().into_owned();
                Some((key, value))
            })
            .collect();
        assert!(
            envs.iter()
                .all(|(k, _)| k != "HTTP_PROXY" && k != "https_proxy"),
            "{envs:?}"
        );
        assert!(
            envs.iter().any(|(k, v)| k == "PATH" && v == "/usr/bin"),
            "{envs:?}"
        );
    }

    #[test]
    fn device_ish_and_empty_names_are_rejected() {
        let root = Path::new("/tmp/ingest-root");
        assert!(safe_join(root, Path::new("nul")).is_err());
        assert!(safe_join(root, Path::new("")).is_err());
    }
}
