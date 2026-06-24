//! Collection records and filesystem discovery for materialized memberships.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::SourceId;

pub const DEFAULT_COLLECTION_SYNC_MAX_DEPTH: usize = 32;
pub const MAX_COLLECTION_NAME_LEN: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionRecord {
    pub name: String,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    #[serde(default)]
    pub watch_enabled: bool,
    #[serde(default = "default_collection_auto_index_enabled")]
    pub auto_index_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<CollectionSyncReport>,
}

fn default_collection_auto_index_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionRootKind {
    File,
    Directory,
    SymlinkFile,
    SymlinkDirectory,
    Other,
    Missing,
}

impl CollectionRootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::SymlinkFile => "symlink_file",
            Self::SymlinkDirectory => "symlink_directory",
            Self::Other => "other",
            Self::Missing => "missing",
        }
    }

    pub fn from_storage_str(value: &str) -> Self {
        match value {
            "file" => Self::File,
            "directory" => Self::Directory,
            "symlink_file" => Self::SymlinkFile,
            "symlink_directory" => Self::SymlinkDirectory,
            "missing" => Self::Missing,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionRoot {
    pub collection_name: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_path: Option<PathBuf>,
    pub kind: CollectionRootKind,
    pub added_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionMember {
    pub collection_name: String,
    pub source_id: SourceId,
    pub logical_path: String,
    pub source_path: PathBuf,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionMemberCandidate {
    pub source_id: SourceId,
    pub logical_path: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncPathInput {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSyncSettings {
    pub ignore_patterns: Vec<String>,
    pub max_depth: usize,
}

impl Default for CollectionSyncSettings {
    fn default() -> Self {
        Self {
            ignore_patterns: Vec::new(),
            max_depth: DEFAULT_COLLECTION_SYNC_MAX_DEPTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionDiscovery {
    pub candidates: Vec<CollectionMemberCandidate>,
    pub skipped: Vec<CollectionSyncSkip>,
    pub scanned_roots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncReport {
    pub member_count: usize,
    pub added: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub scanned_roots: usize,
    pub max_depth: usize,
    #[serde(default)]
    pub skipped: Vec<CollectionSyncSkip>,
}

impl CollectionSyncReport {
    pub fn from_discovery(discovery: &CollectionDiscovery, max_depth: usize) -> Self {
        Self {
            member_count: discovery.candidates.len(),
            added: 0,
            removed: 0,
            unchanged: 0,
            scanned_roots: discovery.scanned_roots,
            max_depth,
            skipped: discovery.skipped.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionStatus {
    pub collection: CollectionRecord,
    pub root_count: usize,
    pub member_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionMembershipDiff {
    pub added: Vec<CollectionMemberCandidate>,
    pub removed: Vec<CollectionMember>,
    pub unchanged: Vec<CollectionMember>,
}

pub fn diff_collection_members(
    old_members: &[CollectionMember],
    new_candidates: &[CollectionMemberCandidate],
) -> CollectionMembershipDiff {
    let old_by_source = old_members
        .iter()
        .map(|member| (member.source_id.clone(), member.clone()))
        .collect::<BTreeMap<_, _>>();
    let new_by_source = new_candidates
        .iter()
        .map(|candidate| (candidate.source_id.clone(), candidate.clone()))
        .collect::<BTreeMap<_, _>>();

    let added = new_by_source
        .iter()
        .filter(|(source_id, _)| !old_by_source.contains_key(*source_id))
        .map(|(_, candidate)| candidate.clone())
        .collect();
    let removed = old_by_source
        .iter()
        .filter(|(source_id, _)| !new_by_source.contains_key(*source_id))
        .map(|(_, member)| member.clone())
        .collect();
    let unchanged = old_by_source
        .iter()
        .filter(|(source_id, _)| new_by_source.contains_key(*source_id))
        .map(|(_, member)| member.clone())
        .collect();

    CollectionMembershipDiff {
        added,
        removed,
        unchanged,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionSyncSkipReason {
    Ignored,
    LoopDetected,
    MaxDepth,
    NotFound,
    Unsupported,
    IoError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncSkip {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
    pub reason: CollectionSyncSkipReason,
    pub message: String,
}

pub fn validate_collection_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_COLLECTION_NAME_LEN {
        anyhow::bail!(
            "collection name must be 1-{MAX_COLLECTION_NAME_LEN} ASCII letters, digits, '.', '_', or '-'"
        );
    }
    if name == "." || name == ".." {
        anyhow::bail!("collection name must not be '.' or '..'");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        anyhow::bail!(
            "collection name must be 1-{MAX_COLLECTION_NAME_LEN} ASCII letters, digits, '.', '_', or '-'"
        );
    }
    Ok(())
}

pub fn resolve_collection_root(path: &Path) -> Result<(CollectionRootKind, Option<PathBuf>)> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect collection root: {}", path.display()))?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolve collection root symlink: {}", path.display()))?;
        let target_metadata = fs::metadata(&canonical)
            .with_context(|| format!("inspect collection root target: {}", canonical.display()))?;
        if target_metadata.is_dir() {
            return Ok((CollectionRootKind::SymlinkDirectory, Some(canonical)));
        }
        if target_metadata.is_file() {
            return Ok((CollectionRootKind::SymlinkFile, Some(canonical)));
        }
        return Ok((CollectionRootKind::Other, Some(canonical)));
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("resolve collection root: {}", path.display()))?;
    if metadata.is_dir() {
        Ok((CollectionRootKind::Directory, Some(canonical)))
    } else if metadata.is_file() {
        Ok((CollectionRootKind::File, Some(canonical)))
    } else {
        Ok((CollectionRootKind::Other, Some(canonical)))
    }
}

pub fn discover_collection_members(
    roots: &[CollectionRoot],
    path_inputs: &[CollectionSyncPathInput],
    settings: &CollectionSyncSettings,
) -> CollectionDiscovery {
    let ignore_rules = CollectionIgnoreRules::new(&settings.ignore_patterns);
    let mut walker = CollectionWalker {
        settings,
        ignore_rules,
        candidates: BTreeMap::new(),
        skipped: Vec::new(),
        active_dirs: BTreeSet::new(),
    };

    for root in roots {
        walker.visit_root(&root.path, None);
    }
    for input in path_inputs {
        walker.visit_root(&input.path, input.logical_path.as_deref());
    }

    let mut candidates = walker.candidates.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));

    CollectionDiscovery {
        candidates,
        skipped: walker.skipped,
        scanned_roots: roots.len() + path_inputs.len(),
    }
}

/// Minimal collection-level ignore matcher.
///
/// Patterns are matched against normalized collection logical paths. A pattern
/// containing `/` matches the whole logical path; a pattern without `/` matches
/// any path segment. `*` and `?` are supported. A trailing `/` matches a
/// directory segment or directory prefix. This small contract is isolated here
/// so a future gitignore-compatible matcher can replace it without changing the
/// collection storage or daemon API.
#[derive(Debug, Clone)]
pub struct CollectionIgnoreRules {
    patterns: Vec<String>,
}

impl CollectionIgnoreRules {
    pub fn new(patterns: &[String]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(|pattern| normalize_ignore_pattern(pattern))
            .collect();
        Self { patterns }
    }

    pub fn is_ignored(&self, logical_path: &str, is_dir: bool) -> bool {
        if logical_path.is_empty() {
            return false;
        }
        self.patterns
            .iter()
            .any(|pattern| ignore_pattern_matches(pattern, logical_path, is_dir))
    }
}

struct CollectionWalker<'a> {
    settings: &'a CollectionSyncSettings,
    ignore_rules: CollectionIgnoreRules,
    candidates: BTreeMap<SourceId, CollectionMemberCandidate>,
    skipped: Vec<CollectionSyncSkip>,
    active_dirs: BTreeSet<PathBuf>,
}

impl CollectionWalker<'_> {
    fn visit_root(&mut self, path: &Path, logical_path: Option<&str>) {
        let logical_path = logical_path.and_then(normalize_logical_path);
        self.visit_path(path, logical_path.as_deref(), 0, true);
        self.active_dirs.clear();
    }

    fn visit_path(&mut self, path: &Path, logical_path: Option<&str>, depth: usize, is_root: bool) {
        if depth > self.settings.max_depth {
            self.skip(
                path,
                logical_path,
                CollectionSyncSkipReason::MaxDepth,
                format!("collection sync depth exceeded {}", self.settings.max_depth),
            );
            return;
        }

        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.skip(
                    path,
                    logical_path,
                    CollectionSyncSkipReason::NotFound,
                    format!("path not found: {}", path.display()),
                );
                return;
            }
            Err(error) => {
                self.skip(
                    path,
                    logical_path,
                    CollectionSyncSkipReason::IoError,
                    format!("inspect path failed: {error}"),
                );
                return;
            }
        };

        if metadata.file_type().is_symlink() {
            self.visit_symlink(path, logical_path, depth, is_root);
        } else if metadata.is_dir() {
            self.visit_dir(path, path, logical_path, depth);
        } else if metadata.is_file() {
            self.visit_file(path, logical_path);
        } else {
            self.skip(
                path,
                logical_path,
                CollectionSyncSkipReason::Unsupported,
                "unsupported filesystem entry".to_string(),
            );
        }
    }

    fn visit_symlink(
        &mut self,
        path: &Path,
        logical_path: Option<&str>,
        depth: usize,
        is_root: bool,
    ) {
        let canonical = match fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(error) => {
                self.skip(
                    path,
                    logical_path,
                    CollectionSyncSkipReason::IoError,
                    format!("resolve symlink failed: {error}"),
                );
                return;
            }
        };
        let target_metadata = match fs::metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.skip(
                    path,
                    logical_path,
                    CollectionSyncSkipReason::IoError,
                    format!("inspect symlink target failed: {error}"),
                );
                return;
            }
        };
        if target_metadata.is_dir() {
            self.visit_dir(path, &canonical, logical_path, depth);
        } else if target_metadata.is_file() {
            let logical_path = logical_path
                .map(ToOwned::to_owned)
                .or_else(|| (!is_root).then(|| file_name_string(path)))
                .unwrap_or_else(|| file_name_string(path));
            self.add_file(&canonical, &logical_path);
        } else {
            self.skip(
                path,
                logical_path,
                CollectionSyncSkipReason::Unsupported,
                "unsupported symlink target".to_string(),
            );
        }
    }

    fn visit_dir(
        &mut self,
        display_path: &Path,
        read_path: &Path,
        logical_path: Option<&str>,
        depth: usize,
    ) {
        if logical_path.is_some_and(|logical| self.ignore_rules.is_ignored(logical, true)) {
            self.skip(
                display_path,
                logical_path,
                CollectionSyncSkipReason::Ignored,
                "ignored by collection pattern".to_string(),
            );
            return;
        }

        let canonical = match fs::canonicalize(read_path) {
            Ok(canonical) => canonical,
            Err(error) => {
                self.skip(
                    display_path,
                    logical_path,
                    CollectionSyncSkipReason::IoError,
                    format!("resolve directory failed: {error}"),
                );
                return;
            }
        };
        if !self.active_dirs.insert(canonical.clone()) {
            self.skip(
                display_path,
                logical_path,
                CollectionSyncSkipReason::LoopDetected,
                format!("directory loop detected at {}", canonical.display()),
            );
            return;
        }

        let mut entries = match fs::read_dir(read_path) {
            Ok(entries) => entries
                .map(|entry| {
                    entry.map_err(|error| {
                        self.skip(
                            read_path,
                            logical_path,
                            CollectionSyncSkipReason::IoError,
                            format!("read directory entry failed: {error}"),
                        );
                    })
                })
                .filter_map(std::result::Result::ok)
                .collect::<Vec<_>>(),
            Err(error) => {
                self.skip(
                    display_path,
                    logical_path,
                    CollectionSyncSkipReason::IoError,
                    format!("read directory failed: {error}"),
                );
                self.active_dirs.remove(&canonical);
                return;
            }
        };
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let child_name = entry.file_name().to_string_lossy().into_owned();
            let child_logical_path = join_logical_path(logical_path, &child_name);
            self.visit_path(&entry.path(), Some(&child_logical_path), depth + 1, false);
        }

        self.active_dirs.remove(&canonical);
    }

    fn visit_file(&mut self, path: &Path, logical_path: Option<&str>) {
        let logical_path = logical_path
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| file_name_string(path));
        let canonical = match fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(error) => {
                self.skip(
                    path,
                    Some(&logical_path),
                    CollectionSyncSkipReason::IoError,
                    format!("resolve file failed: {error}"),
                );
                return;
            }
        };
        self.add_file(&canonical, &logical_path);
    }

    fn add_file(&mut self, canonical_path: &Path, logical_path: &str) {
        let logical_path = normalize_logical_path(logical_path).unwrap_or_else(|| {
            canonical_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "source".to_string())
        });
        if self.ignore_rules.is_ignored(&logical_path, false) {
            self.skip(
                canonical_path,
                Some(&logical_path),
                CollectionSyncSkipReason::Ignored,
                "ignored by collection pattern".to_string(),
            );
            return;
        }
        let source_id = SourceId::from_path(canonical_path);
        let candidate = CollectionMemberCandidate {
            source_id: source_id.clone(),
            logical_path,
            source_path: canonical_path.to_path_buf(),
        };
        self.candidates
            .entry(source_id)
            .and_modify(|existing| {
                if candidate.logical_path < existing.logical_path {
                    *existing = candidate.clone();
                }
            })
            .or_insert(candidate);
    }

    fn skip(
        &mut self,
        path: &Path,
        logical_path: Option<&str>,
        reason: CollectionSyncSkipReason,
        message: String,
    ) {
        self.skipped.push(CollectionSyncSkip {
            path: path.display().to_string(),
            logical_path: logical_path.map(ToOwned::to_owned),
            reason,
            message,
        });
    }
}

fn normalize_ignore_pattern(pattern: &str) -> Option<String> {
    let pattern = pattern.trim().replace('\\', "/");
    let directory_only = pattern.ends_with('/');
    let pattern = pattern.trim_matches('/');
    if pattern.is_empty() {
        return None;
    }
    Some(if directory_only {
        format!("{pattern}/")
    } else {
        pattern.to_string()
    })
}

fn ignore_pattern_matches(pattern: &str, logical_path: &str, is_dir: bool) -> bool {
    let directory_only = pattern.ends_with('/');
    if directory_only && !is_dir {
        return false;
    }
    let pattern = pattern.trim_end_matches('/');
    if pattern.contains('/') {
        return wildcard_match(pattern, logical_path)
            || (is_dir && logical_path.starts_with(&format!("{pattern}/")));
    }
    logical_path
        .split('/')
        .any(|segment| wildcard_match(pattern, segment))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut dp = vec![false; value.len() + 1];
    dp[0] = true;
    for &token in pattern {
        let mut next = vec![false; value.len() + 1];
        match token {
            b'*' => {
                next[0] = dp[0];
                for index in 1..=value.len() {
                    next[index] = dp[index] || next[index - 1];
                }
            }
            b'?' => {
                if !value.is_empty() {
                    next[1..=value.len()].copy_from_slice(&dp[..value.len()]);
                }
            }
            literal => {
                for index in 1..=value.len() {
                    next[index] = dp[index - 1] && value[index - 1] == literal;
                }
            }
        }
        dp = next;
    }
    dp[value.len()]
}

fn join_logical_path(parent: Option<&str>, child: &str) -> String {
    let child = child.replace('\\', "/");
    match parent.filter(|value| !value.is_empty()) {
        Some(parent) => format!("{parent}/{child}"),
        None => child,
    }
}

fn normalize_logical_path(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;

    fn root(path: &Path) -> CollectionRoot {
        let (kind, canonical_path) = resolve_collection_root(path).unwrap();
        CollectionRoot {
            collection_name: "articles".into(),
            path: path.to_path_buf(),
            canonical_path,
            kind,
            added_at: "1".into(),
            updated_at: "1".into(),
        }
    }

    fn discover(root_path: &Path, ignore_patterns: &[&str]) -> CollectionDiscovery {
        discover_collection_members(
            &[root(root_path)],
            &[],
            &CollectionSyncSettings {
                ignore_patterns: ignore_patterns
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                max_depth: DEFAULT_COLLECTION_SYNC_MAX_DEPTH,
            },
        )
    }

    #[test]
    fn discovers_directory_root_members_with_logical_paths() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested").join("a.md"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();

        let discovery = discover(dir.path(), &[]);
        let logical = discovery
            .candidates
            .iter()
            .map(|candidate| candidate.logical_path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(logical, vec!["b.txt", "nested/a.md"]);
        assert!(discovery.skipped.is_empty());
    }

    #[test]
    fn discovers_file_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("article.md");
        fs::write(&path, "article").unwrap();

        let discovery = discover(&path, &[]);

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].logical_path, "article.md");
        assert_eq!(
            discovery.candidates[0].source_id,
            SourceId::from_path(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlink_to_file() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.md");
        let link = dir.path().join("linked.md");
        fs::write(&target, "article").unwrap();
        unix_fs::symlink(&target, &link).unwrap();

        let discovery = discover(&link, &[]);

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].logical_path, "linked.md");
        assert_eq!(
            discovery.candidates[0].source_path,
            fs::canonicalize(&target).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_file_target_change_changes_candidate_source() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.md");
        let second = dir.path().join("second.md");
        let link = dir.path().join("linked.md");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        unix_fs::symlink(&first, &link).unwrap();
        let first_discovery = discover(&link, &[]);
        fs::remove_file(&link).unwrap();
        unix_fs::symlink(&second, &link).unwrap();

        let second_discovery = discover(&link, &[]);

        assert_eq!(first_discovery.candidates.len(), 1);
        assert_eq!(second_discovery.candidates.len(), 1);
        assert_ne!(
            first_discovery.candidates[0].source_id,
            second_discovery.candidates[0].source_id
        );
        assert_eq!(
            second_discovery.candidates[0].source_path,
            fs::canonicalize(&second).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlink_to_directory() {
        let dir = tempdir().unwrap();
        let target_dir = dir.path().join("target");
        let link_dir = dir.path().join("linked");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("article.md"), "article").unwrap();
        unix_fs::symlink(&target_dir, &link_dir).unwrap();

        let discovery = discover(&link_dir, &[]);

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].logical_path, "article.md");
    }

    #[cfg(unix)]
    #[test]
    fn follows_nested_symlink_directory() {
        let dir = tempdir().unwrap();
        let target_dir = dir.path().join("target");
        let nested_dir = dir.path().join("nested");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(target_dir.join("article.md"), "article").unwrap();
        unix_fs::symlink(&target_dir, nested_dir.join("target-link")).unwrap();

        let discovery = discover(dir.path(), &[]);

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(
            discovery.candidates[0].logical_path,
            "nested/target-link/article.md"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_directory_loop() {
        let dir = tempdir().unwrap();
        let child = dir.path().join("child");
        fs::create_dir_all(&child).unwrap();
        unix_fs::symlink(dir.path(), child.join("loop")).unwrap();

        let discovery = discover(dir.path(), &[]);

        assert!(discovery
            .skipped
            .iter()
            .any(|skip| skip.reason == CollectionSyncSkipReason::LoopDetected));
    }

    #[test]
    fn applies_collection_ignore_patterns() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("drafts")).unwrap();
        fs::write(dir.path().join("drafts").join("skip.md"), "skip").unwrap();
        fs::write(dir.path().join("keep.md"), "keep").unwrap();
        fs::write(dir.path().join("notes.tmp"), "tmp").unwrap();

        let discovery = discover(dir.path(), &["drafts/", "*.tmp"]);

        let logical = discovery
            .candidates
            .iter()
            .map(|candidate| candidate.logical_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(logical, vec!["keep.md"]);
        assert_eq!(discovery.skipped.len(), 2);
    }

    #[test]
    fn stdin_path_inputs_preserve_requested_logical_paths() {
        let dir = tempdir().unwrap();
        let articles = dir.path().join("articles");
        fs::create_dir_all(&articles).unwrap();
        let path = articles.join("Areskapitalon.md");
        fs::write(&path, "article").unwrap();

        let discovery = discover_collection_members(
            &[],
            &[CollectionSyncPathInput {
                path,
                logical_path: Some("../drafts/articles/articles/Areskapitalon.md".into()),
            }],
            &CollectionSyncSettings::default(),
        );

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(
            discovery.candidates[0].logical_path,
            "../drafts/articles/articles/Areskapitalon.md"
        );
    }

    #[test]
    fn enforces_max_depth() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("one").join("two")).unwrap();
        fs::write(
            dir.path().join("one").join("two").join("article.md"),
            "article",
        )
        .unwrap();

        let discovery = discover_collection_members(
            &[root(dir.path())],
            &[],
            &CollectionSyncSettings {
                ignore_patterns: Vec::new(),
                max_depth: 1,
            },
        );

        assert!(discovery.candidates.is_empty());
        assert!(discovery
            .skipped
            .iter()
            .any(|skip| skip.reason == CollectionSyncSkipReason::MaxDepth));
    }

    #[test]
    fn diffs_collection_members_by_source_id() {
        let old = vec![
            CollectionMember {
                collection_name: "articles".into(),
                source_id: SourceId("src-1".into()),
                logical_path: "one.md".into(),
                source_path: PathBuf::from("/tmp/one.md"),
                updated_at: "1".into(),
            },
            CollectionMember {
                collection_name: "articles".into(),
                source_id: SourceId("src-2".into()),
                logical_path: "two.md".into(),
                source_path: PathBuf::from("/tmp/two.md"),
                updated_at: "1".into(),
            },
        ];
        let new = vec![
            CollectionMemberCandidate {
                source_id: SourceId("src-2".into()),
                logical_path: "renamed.md".into(),
                source_path: PathBuf::from("/tmp/two.md"),
            },
            CollectionMemberCandidate {
                source_id: SourceId("src-3".into()),
                logical_path: "three.md".into(),
                source_path: PathBuf::from("/tmp/three.md"),
            },
        ];

        let diff = diff_collection_members(&old, &new);

        assert_eq!(diff.added[0].source_id, SourceId("src-3".into()));
        assert_eq!(diff.removed[0].source_id, SourceId("src-1".into()));
        assert_eq!(diff.unchanged[0].source_id, SourceId("src-2".into()));
    }
}
