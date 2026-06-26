use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::types::EmbeddingProfileId;

pub const DEFAULT_INDEX_GC_RETAIN_PREVIOUS_GENERATIONS: usize = 2;
pub const DEFAULT_INDEX_GC_STALE_STAGING_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcConfig {
    #[serde(default = "default_retain_previous_generations")]
    pub retain_previous_generations: usize,
    #[serde(default = "default_stale_staging_seconds")]
    pub stale_staging_seconds: u64,
}

impl Default for IndexGcConfig {
    fn default() -> Self {
        Self {
            retain_previous_generations: default_retain_previous_generations(),
            stale_staging_seconds: default_stale_staging_seconds(),
        }
    }
}

impl IndexGcConfig {
    pub fn policy(&self) -> IndexGcPolicy {
        IndexGcPolicy {
            retain_previous_generations: self.retain_previous_generations,
            stale_staging_age: Duration::from_secs(self.stale_staging_seconds),
        }
    }
}

fn default_retain_previous_generations() -> usize {
    DEFAULT_INDEX_GC_RETAIN_PREVIOUS_GENERATIONS
}

fn default_stale_staging_seconds() -> u64 {
    DEFAULT_INDEX_GC_STALE_STAGING_SECONDS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexGcPolicy {
    pub retain_previous_generations: usize,
    pub stale_staging_age: Duration,
}

impl Default for IndexGcPolicy {
    fn default() -> Self {
        IndexGcConfig::default().policy()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexGcArtifactKind {
    Generation,
    Staging,
}

impl IndexGcArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Staging => "staging",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcPlanEntry {
    pub path: PathBuf,
    pub kind: IndexGcArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    pub approximate_bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcSkippedEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IndexGcArtifactKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcPlan {
    #[serde(default)]
    pub entries: Vec<IndexGcPlanEntry>,
    #[serde(default)]
    pub skipped: Vec<IndexGcSkippedEntry>,
    pub approximate_reclaim_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcApplyReport {
    #[serde(default)]
    pub removed: Vec<IndexGcPlanEntry>,
    pub reclaimed_bytes: u64,
}

pub fn plan_index_gc(data_dir: &Path, store: &Store, policy: IndexGcPolicy) -> Result<IndexGcPlan> {
    plan_index_gc_at(data_dir, store, policy, SystemTime::now())
}

pub fn apply_index_gc(
    data_dir: &Path,
    store: &Store,
    policy: IndexGcPolicy,
) -> Result<(IndexGcPlan, IndexGcApplyReport)> {
    let plan = plan_index_gc(data_dir, store, policy)?;
    let report = apply_index_gc_plan(data_dir, &plan)?;
    Ok((plan, report))
}

pub fn apply_index_gc_plan(data_dir: &Path, plan: &IndexGcPlan) -> Result<IndexGcApplyReport> {
    let index_root = index_root_dir(data_dir);
    let mut report = IndexGcApplyReport::default();
    for entry in &plan.entries {
        if remove_planned_entry(&index_root, entry)? {
            report.reclaimed_bytes = report
                .reclaimed_bytes
                .saturating_add(entry.approximate_bytes);
            report.removed.push(entry.clone());
        }
    }
    Ok(report)
}

pub(crate) fn plan_index_gc_at(
    data_dir: &Path,
    store: &Store,
    policy: IndexGcPolicy,
    now: SystemTime,
) -> Result<IndexGcPlan> {
    let index_root = index_root_dir(data_dir);
    let mut plan = IndexGcPlan::default();
    if !index_root.exists() {
        return Ok(plan);
    }
    if is_symlink(&index_root)? {
        plan.skipped.push(IndexGcSkippedEntry {
            path: index_root,
            kind: None,
            profile_id: None,
            generation: None,
            reason: "index root is a symlink; gc will not follow it".to_string(),
        });
        return Ok(plan);
    }

    let current_generations = current_generation_map(store)?;
    plan_profile_generations(
        &index_root,
        &current_generations,
        policy.retain_previous_generations,
        &mut plan,
    )?;
    plan_staging_directories(&index_root, policy.stale_staging_age, now, &mut plan)?;
    plan.entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    plan.skipped
        .sort_by(|left, right| left.path.cmp(&right.path));
    plan.approximate_reclaim_bytes = plan.entries.iter().fold(0_u64, |sum, entry| {
        sum.saturating_add(entry.approximate_bytes)
    });
    Ok(plan)
}

fn current_generation_map(store: &Store) -> Result<BTreeMap<String, u64>> {
    let mut current = BTreeMap::new();
    for generation in store.profile_index_generations()? {
        current.insert(
            generation.profile_id.as_str().to_string(),
            generation.generation,
        );
    }
    Ok(current)
}

fn plan_profile_generations(
    index_root: &Path,
    current_generations: &BTreeMap<String, u64>,
    retain_previous_generations: usize,
    plan: &mut IndexGcPlan,
) -> Result<()> {
    let profiles_root = index_root.join("profiles");
    if !profiles_root.exists() {
        return Ok(());
    }
    if is_symlink(&profiles_root)? {
        plan.skipped.push(IndexGcSkippedEntry {
            path: profiles_root,
            kind: None,
            profile_id: None,
            generation: None,
            reason: "profiles root is a symlink; gc will not follow it".to_string(),
        });
        return Ok(());
    }

    for profile_entry in fs::read_dir(&profiles_root)
        .with_context(|| format!("read index profiles root: {}", profiles_root.display()))?
    {
        let profile_entry = profile_entry?;
        let profile_path = profile_entry.path();
        let profile_name = profile_entry.file_name().to_string_lossy().into_owned();
        if is_symlink(&profile_path)? {
            plan.skipped.push(IndexGcSkippedEntry {
                path: profile_path,
                kind: None,
                profile_id: Some(profile_name),
                generation: None,
                reason: "profile index directory is a symlink; gc will not follow it".to_string(),
            });
            continue;
        }
        if !profile_entry.file_type()?.is_dir() {
            continue;
        }
        if EmbeddingProfileId::new(profile_name.clone()).is_err() {
            plan.skipped.push(IndexGcSkippedEntry {
                path: profile_path,
                kind: None,
                profile_id: Some(profile_name),
                generation: None,
                reason: "profile directory name is not a valid embedding profile id".to_string(),
            });
            continue;
        }
        plan_profile_generation_directories(
            &profile_path,
            &profile_name,
            current_generations,
            retain_previous_generations,
            plan,
        )?;
    }
    Ok(())
}

fn plan_profile_generation_directories(
    profile_path: &Path,
    profile_name: &str,
    current_generations: &BTreeMap<String, u64>,
    retain_previous_generations: usize,
    plan: &mut IndexGcPlan,
) -> Result<()> {
    let Some(current_generation) = current_generations.get(profile_name).copied() else {
        plan.skipped.push(IndexGcSkippedEntry {
            path: profile_path.to_path_buf(),
            kind: None,
            profile_id: Some(profile_name.to_string()),
            generation: None,
            reason:
                "profile has no embedding_profile_index_meta row; current generation is unknown"
                    .to_string(),
        });
        return Ok(());
    };

    let generation_dirs = generation_dirs(profile_path, profile_name, plan)?;
    let current_path = profile_path.join(format!("gen-{current_generation}"));
    if !generation_dirs
        .iter()
        .any(|dir| dir.generation == current_generation)
    {
        plan.skipped.push(IndexGcSkippedEntry {
            path: current_path,
            kind: Some(IndexGcArtifactKind::Generation),
            profile_id: Some(profile_name.to_string()),
            generation: Some(current_generation),
            reason: "metadata current generation is missing on disk; skipping this profile"
                .to_string(),
        });
        for dir in generation_dirs {
            plan.skipped.push(IndexGcSkippedEntry {
                path: dir.path,
                kind: Some(IndexGcArtifactKind::Generation),
                profile_id: Some(profile_name.to_string()),
                generation: Some(dir.generation),
                reason: "profile current generation is missing; generation cleanup is conservative"
                    .to_string(),
            });
        }
        return Ok(());
    }

    let mut retained = BTreeSet::new();
    retained.insert(current_generation);
    for generation in generation_dirs
        .iter()
        .filter(|dir| dir.generation < current_generation)
        .map(|dir| dir.generation)
        .rev()
        .take(retain_previous_generations)
    {
        retained.insert(generation);
    }

    for dir in generation_dirs {
        if retained.contains(&dir.generation) {
            continue;
        }
        if dir.generation > current_generation {
            plan.skipped.push(IndexGcSkippedEntry {
                path: dir.path,
                kind: Some(IndexGcArtifactKind::Generation),
                profile_id: Some(profile_name.to_string()),
                generation: Some(dir.generation),
                reason: "generation is newer than metadata current generation; skipping"
                    .to_string(),
            });
            continue;
        }
        plan.entries.push(IndexGcPlanEntry {
            approximate_bytes: approximate_dir_size(&dir.path)?,
            path: dir.path,
            kind: IndexGcArtifactKind::Generation,
            profile_id: Some(profile_name.to_string()),
            generation: Some(dir.generation),
            reason: format!(
                "older than current generation {current_generation} plus {retain_previous_generations} retained previous generation(s)"
            ),
        });
    }
    Ok(())
}

fn generation_dirs(
    profile_path: &Path,
    profile_name: &str,
    plan: &mut IndexGcPlan,
) -> Result<Vec<GenerationDir>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(profile_path)
        .with_context(|| format!("read profile index dir: {}", profile_path.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(generation_text) = file_name.strip_prefix("gen-") else {
            continue;
        };
        let generation = match generation_text.parse::<u64>() {
            Ok(generation) => generation,
            Err(_) => {
                plan.skipped.push(IndexGcSkippedEntry {
                    path,
                    kind: Some(IndexGcArtifactKind::Generation),
                    profile_id: Some(profile_name.to_string()),
                    generation: None,
                    reason: "generation directory suffix is not an unsigned integer".to_string(),
                });
                continue;
            }
        };
        if is_symlink(&path)? {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Generation),
                profile_id: Some(profile_name.to_string()),
                generation: Some(generation),
                reason: "generation path is a symlink; gc will not follow it".to_string(),
            });
            continue;
        }
        if !entry.file_type()?.is_dir() {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Generation),
                profile_id: Some(profile_name.to_string()),
                generation: Some(generation),
                reason: "generation path is not a directory".to_string(),
            });
            continue;
        }
        dirs.push(GenerationDir { path, generation });
    }
    dirs.sort_by_key(|dir| dir.generation);
    Ok(dirs)
}

fn plan_staging_directories(
    index_root: &Path,
    stale_staging_age: Duration,
    now: SystemTime,
    plan: &mut IndexGcPlan,
) -> Result<()> {
    for entry in fs::read_dir(index_root)
        .with_context(|| format!("read index root: {}", index_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !file_name.starts_with("staging-") {
            continue;
        }
        if is_symlink(&path)? {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Staging),
                profile_id: None,
                generation: None,
                reason: "staging path is a symlink; gc will not follow it".to_string(),
            });
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_dir() {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Staging),
                profile_id: None,
                generation: None,
                reason: "staging path is not a directory".to_string(),
            });
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Staging),
                profile_id: None,
                generation: None,
                reason: "staging directory modified time is unavailable".to_string(),
            });
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Staging),
                profile_id: None,
                generation: None,
                reason: "staging directory modified time is in the future".to_string(),
            });
            continue;
        };
        if age < stale_staging_age {
            plan.skipped.push(IndexGcSkippedEntry {
                path,
                kind: Some(IndexGcArtifactKind::Staging),
                profile_id: None,
                generation: None,
                reason: format!(
                    "staging directory age {}s is below stale threshold {}s",
                    age.as_secs(),
                    stale_staging_age.as_secs()
                ),
            });
            continue;
        }
        plan.entries.push(IndexGcPlanEntry {
            approximate_bytes: approximate_dir_size(&path)?,
            path,
            kind: IndexGcArtifactKind::Staging,
            profile_id: None,
            generation: None,
            reason: format!(
                "staging directory is at least {}s old",
                stale_staging_age.as_secs()
            ),
        });
    }
    Ok(())
}

fn remove_planned_entry(index_root: &Path, entry: &IndexGcPlanEntry) -> Result<bool> {
    let removal_path = validate_planned_entry(index_root, entry)?;
    match fs::remove_dir_all(&removal_path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("remove index gc path: {}", removal_path.display()))
        }
    }
}

fn validate_planned_entry(index_root: &Path, entry: &IndexGcPlanEntry) -> Result<PathBuf> {
    validate_normal_path_components(&entry.path)?;
    validate_artifact_shape(entry)?;
    validate_artifact_location(index_root, entry)?;
    let removal_path = absolute_lexical(&entry.path)?;
    validate_contained_path(index_root, &removal_path)?;
    validate_no_symlink_components(index_root, &removal_path)?;
    let metadata = match fs::symlink_metadata(&removal_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(removal_path),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("inspect index gc path: {}", removal_path.display()));
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to remove symlink index gc path: {}",
            removal_path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "refusing to remove non-directory index gc path: {}",
            removal_path.display()
        );
    }
    Ok(removal_path)
}

fn validate_artifact_location(index_root: &Path, entry: &IndexGcPlanEntry) -> Result<()> {
    let expected = match entry.kind {
        IndexGcArtifactKind::Generation => {
            let profile_id = entry
                .profile_id
                .as_deref()
                .context("generation gc entry missing profile_id")?;
            let generation = entry
                .generation
                .context("generation gc entry missing generation")?;
            index_root
                .join("profiles")
                .join(profile_id)
                .join(format!("gen-{generation}"))
        }
        IndexGcArtifactKind::Staging => {
            let name = entry
                .path
                .file_name()
                .context("staging gc entry missing path file name")?;
            index_root.join(name)
        }
    };
    if absolute_lexical(&entry.path)? != absolute_lexical(&expected)? {
        bail!(
            "index gc entry path does not match expected artifact location: {}",
            entry.path.display()
        );
    }
    Ok(())
}

fn validate_artifact_shape(entry: &IndexGcPlanEntry) -> Result<()> {
    let name = entry
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match entry.kind {
        IndexGcArtifactKind::Generation => {
            let profile_id = entry
                .profile_id
                .as_deref()
                .context("generation gc entry missing profile_id")?;
            let generation = entry
                .generation
                .context("generation gc entry missing generation")?;
            if name != format!("gen-{generation}") {
                bail!(
                    "generation gc entry path does not match generation: {}",
                    entry.path.display()
                );
            }
            let parent_profile = entry
                .path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if parent_profile != profile_id {
                bail!(
                    "generation gc entry path does not match profile id: {}",
                    entry.path.display()
                );
            }
        }
        IndexGcArtifactKind::Staging => {
            if !name.starts_with("staging-") {
                bail!(
                    "staging gc entry path does not start with staging-: {}",
                    entry.path.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_normal_path_components(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir => {
                bail!(
                    "refusing to remove index gc path with non-normal component: {}",
                    path.display()
                );
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

fn validate_contained_path(index_root: &Path, path: &Path) -> Result<()> {
    let root = absolute_lexical(index_root)?;
    let candidate = absolute_lexical(path)?;
    if candidate == root || !candidate.starts_with(&root) {
        bail!(
            "refusing to remove path outside index root: {} (root {})",
            path.display(),
            index_root.display()
        );
    }
    Ok(())
}

fn validate_no_symlink_components(index_root: &Path, path: &Path) -> Result<()> {
    let root = absolute_lexical(index_root)?;
    let candidate = absolute_lexical(path)?;
    let relative = candidate.strip_prefix(&root).with_context(|| {
        format!(
            "index gc path is not under root: {} (root {})",
            path.display(),
            index_root.display()
        )
    })?;
    let mut cursor = root;
    if is_symlink(&cursor)? {
        bail!(
            "refusing to remove through symlink index root: {}",
            cursor.display()
        );
    }
    for component in relative.components() {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing to remove path with symlink component: {}",
                    cursor.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("inspect index gc path: {}", cursor.display()));
            }
        }
    }
    Ok(())
}

fn approximate_dir_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect index artifact: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    let mut total = metadata.len();
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("read index artifact: {}", path.display()))?
        {
            total = total.saturating_add(approximate_dir_size(&entry?.path())?);
        }
    }
    Ok(total)
}

fn is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("inspect path: {}", path.display())),
    }
}

fn index_root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes")
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for index gc path validation")?
            .join(path)
    };
    Ok(normalize_lexical(&path))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationDir {
    path: PathBuf,
    generation: u64,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use tempfile::tempdir;

    use super::*;
    use crate::store::{EmbeddingProfileConfig, Store};

    fn test_profile_config() -> EmbeddingProfileConfig<'static> {
        EmbeddingProfileConfig {
            provider: "test",
            model: "test-model",
            dimension: 2,
            normalize: true,
            endpoint_identity: None,
            requested_model: None,
            served_model: None,
            max_context_tokens: None,
            dtype: None,
            quantization: None,
            weight_identity: None,
            chunker_version: "parent-child-v2",
            child_target_tokens: 300,
            child_overlap_tokens: 80,
            parent_children_count: 5,
            embedding_input_budget_tokens: None,
            query_instruction: "",
            document_instruction: "",
        }
    }

    fn ensure_profile_generation(store: &Store, profile_id: &str, generation: u64) {
        let profile = EmbeddingProfileId::new(profile_id).unwrap();
        store
            .ensure_embedding_profile(&profile, test_profile_config())
            .unwrap();
        let mut current = store.index_generation_for_profile(&profile).unwrap();
        while current < generation {
            store
                .replace_all_vector_documents_for_profile(&profile, &[])
                .unwrap();
            current = store.index_generation_for_profile(&profile).unwrap();
        }
        assert_eq!(
            store.index_generation_for_profile(&profile).unwrap(),
            generation
        );
    }

    fn write_generation(
        data_dir: &Path,
        profile_id: &str,
        generation: u64,
        bytes: &[u8],
    ) -> PathBuf {
        let path = data_dir
            .join("indexes")
            .join("profiles")
            .join(profile_id)
            .join(format!("gen-{generation}"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("vectors.hnsw"), bytes).unwrap();
        path
    }

    fn write_staging(data_dir: &Path, name: &str) -> PathBuf {
        let path = data_dir.join("indexes").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("vectors.hnsw"), b"staging").unwrap();
        path
    }

    fn test_policy(previous: usize, stale_secs: u64) -> IndexGcPolicy {
        IndexGcPolicy {
            retain_previous_generations: previous,
            stale_staging_age: Duration::from_secs(stale_secs),
        }
    }

    #[test]
    fn current_generation_is_never_planned() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 3);
        write_generation(tempdir.path(), "default", 1, b"old");
        write_generation(tempdir.path(), "default", 2, b"previous");
        let current = write_generation(tempdir.path(), "default", 3, b"current");

        let plan =
            plan_index_gc_at(tempdir.path(), &store, test_policy(0, 1), SystemTime::now()).unwrap();

        assert_eq!(plan.entries.len(), 2);
        assert!(!plan.entries.iter().any(|entry| entry.path == current));
        assert!(current.exists());
    }

    #[test]
    fn retention_keeps_current_plus_configured_previous_generations() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 5);
        for generation in 1..=5 {
            write_generation(tempdir.path(), "default", generation, b"index");
        }

        let plan =
            plan_index_gc_at(tempdir.path(), &store, test_policy(2, 1), SystemTime::now()).unwrap();
        let planned = plan
            .entries
            .iter()
            .map(|entry| entry.generation.unwrap())
            .collect::<Vec<_>>();

        assert_eq!(planned, vec![1, 2]);
    }

    #[test]
    fn multiple_profiles_keep_each_profile_current_generation() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 3);
        ensure_profile_generation(&store, "alt", 2);
        for generation in 1..=3 {
            write_generation(tempdir.path(), "default", generation, b"default");
        }
        for generation in 1..=2 {
            write_generation(tempdir.path(), "alt", generation, b"alt");
        }

        let plan =
            plan_index_gc_at(tempdir.path(), &store, test_policy(0, 1), SystemTime::now()).unwrap();
        let planned = plan
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.profile_id.as_deref().unwrap().to_string(),
                    entry.generation.unwrap(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            planned,
            vec![
                ("alt".to_string(), 1),
                ("default".to_string(), 1),
                ("default".to_string(), 2),
            ]
        );
    }

    #[test]
    fn missing_current_generation_skips_profile_conservatively() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 3);
        write_generation(tempdir.path(), "default", 1, b"old");
        write_generation(tempdir.path(), "default", 2, b"previous");

        let plan =
            plan_index_gc_at(tempdir.path(), &store, test_policy(0, 1), SystemTime::now()).unwrap();

        assert!(plan.entries.is_empty());
        assert!(plan
            .skipped
            .iter()
            .any(|entry| entry.reason.contains("current generation is missing")));
    }

    #[cfg(unix)]
    #[test]
    fn stale_staging_is_planned_but_fresh_staging_is_retained() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let stale = write_staging(tempdir.path(), "staging-10-old");
        let fresh = write_staging(tempdir.path(), "staging-10-fresh");
        set_mtime_unix(&stale, 1);
        let now = SystemTime::now();

        let plan = plan_index_gc_at(tempdir.path(), &store, test_policy(0, 60), now).unwrap();

        assert!(plan.entries.iter().any(|entry| entry.path == stale));
        assert!(plan.skipped.iter().any(|entry| entry.path == fresh));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_generation_is_not_followed_or_deleted() {
        let tempdir = tempdir().unwrap();
        let outside = tempdir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"outside").unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 2);
        write_generation(tempdir.path(), "default", 2, b"current");
        let link = tempdir
            .path()
            .join("indexes")
            .join("profiles")
            .join("default")
            .join("gen-1");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let plan =
            plan_index_gc_at(tempdir.path(), &store, test_policy(0, 1), SystemTime::now()).unwrap();
        let report = apply_index_gc_plan(tempdir.path(), &plan).unwrap();

        assert!(plan.entries.is_empty());
        assert!(report.removed.is_empty());
        assert!(outside.join("keep.txt").exists());
        assert!(link.exists());
    }

    #[test]
    fn apply_deletes_planned_safe_artifacts_idempotently() {
        let tempdir = tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        ensure_profile_generation(&store, "default", 2);
        let old = write_generation(tempdir.path(), "default", 1, b"old");
        let current = write_generation(tempdir.path(), "default", 2, b"current");

        let (plan, report) = apply_index_gc(tempdir.path(), &store, test_policy(0, 1)).unwrap();
        let second_report = apply_index_gc_plan(tempdir.path(), &plan).unwrap();

        assert_eq!(report.removed.len(), 1);
        assert!(!old.exists());
        assert!(current.exists());
        assert!(second_report.removed.is_empty());
    }

    #[test]
    fn apply_rejects_generation_outside_profile_location() {
        let tempdir = tempdir().unwrap();
        let misplaced = tempdir
            .path()
            .join("indexes")
            .join("not-profiles")
            .join("default")
            .join("gen-1");
        fs::create_dir_all(&misplaced).unwrap();
        fs::write(misplaced.join("vectors.hnsw"), b"keep").unwrap();
        let plan = IndexGcPlan {
            entries: vec![IndexGcPlanEntry {
                path: misplaced.clone(),
                kind: IndexGcArtifactKind::Generation,
                profile_id: Some("default".to_string()),
                generation: Some(1),
                approximate_bytes: 1,
                reason: "test".to_string(),
            }],
            skipped: Vec::new(),
            approximate_reclaim_bytes: 1,
        };

        let error = apply_index_gc_plan(tempdir.path(), &plan).unwrap_err();

        assert!(error
            .to_string()
            .contains("does not match expected artifact location"));
        assert!(misplaced.join("vectors.hnsw").exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_staging_path_with_symlink_parent_detour() {
        let tempdir = tempdir().unwrap();
        let index_root = tempdir.path().join("indexes");
        fs::create_dir_all(&index_root).unwrap();
        let outside = tempdir.path().join("outside");
        let anchor = outside.join("anchor");
        let victim = outside.join("staging-victim");
        fs::create_dir_all(&anchor).unwrap();
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("keep.txt"), b"outside").unwrap();
        std::os::unix::fs::symlink(&anchor, index_root.join("link")).unwrap();
        let crafted = index_root.join("link").join("..").join("staging-victim");
        let plan = IndexGcPlan {
            entries: vec![IndexGcPlanEntry {
                path: crafted,
                kind: IndexGcArtifactKind::Staging,
                profile_id: None,
                generation: None,
                approximate_bytes: 1,
                reason: "test".to_string(),
            }],
            skipped: Vec::new(),
            approximate_reclaim_bytes: 1,
        };

        let error = apply_index_gc_plan(tempdir.path(), &plan).unwrap_err();

        assert!(error.to_string().contains("non-normal component"));
        assert!(victim.join("keep.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_generation_path_with_symlink_parent_detour() {
        let tempdir = tempdir().unwrap();
        let index_root = tempdir.path().join("indexes");
        fs::create_dir_all(&index_root).unwrap();
        let outside = tempdir.path().join("outside");
        let anchor = outside.join("anchor");
        let victim = outside.join("profiles").join("default").join("gen-1");
        fs::create_dir_all(&anchor).unwrap();
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("keep.txt"), b"outside").unwrap();
        std::os::unix::fs::symlink(&anchor, index_root.join("link")).unwrap();
        let crafted = index_root
            .join("link")
            .join("..")
            .join("profiles")
            .join("default")
            .join("gen-1");
        let plan = IndexGcPlan {
            entries: vec![IndexGcPlanEntry {
                path: crafted,
                kind: IndexGcArtifactKind::Generation,
                profile_id: Some("default".to_string()),
                generation: Some(1),
                approximate_bytes: 1,
                reason: "test".to_string(),
            }],
            skipped: Vec::new(),
            approximate_reclaim_bytes: 1,
        };

        let error = apply_index_gc_plan(tempdir.path(), &plan).unwrap_err();

        assert!(error.to_string().contains("non-normal component"));
        assert!(victim.join("keep.txt").exists());
    }

    #[cfg(unix)]
    fn set_mtime_unix(path: &Path, secs: i64) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        // SAFETY: path is a valid NUL-terminated CString and times points to two initialized timespec values.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0);
    }
}
