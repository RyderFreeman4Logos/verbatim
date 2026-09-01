use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::store::{EmbeddingProfileStorageCounts, Store};
use crate::types::EmbeddingProfileId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileDeletePlan {
    pub profile_id: String,
    pub active_profile: bool,
    pub sqlite: EmbeddingProfileStorageCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<IndexProfileArtifactPlan>,
    #[serde(default)]
    pub skipped: Vec<IndexProfileDeleteSkippedEntry>,
    pub approximate_reclaim_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileArtifactPlan {
    pub path: PathBuf,
    pub approximate_bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileDeleteSkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileDeleteApplyReport {
    pub sqlite: EmbeddingProfileStorageCounts,
    #[serde(default)]
    pub removed_artifacts: Vec<IndexProfileArtifactPlan>,
    pub reclaimed_bytes: u64,
}

pub fn plan_index_profile_delete(
    data_dir: &Path,
    store: &Store,
    profile_id: &EmbeddingProfileId,
    active_profile_id: &EmbeddingProfileId,
) -> Result<IndexProfileDeletePlan> {
    let sqlite = store.embedding_profile_storage_counts(profile_id)?;
    let mut plan = IndexProfileDeletePlan {
        profile_id: profile_id.as_str().to_string(),
        active_profile: profile_id == active_profile_id,
        sqlite,
        artifact: None,
        skipped: Vec::new(),
        approximate_reclaim_bytes: 0,
    };

    let path = profile_index_root_dir(data_dir, profile_id);
    if let Some(skipped) = unsafe_profile_indexes_root(data_dir)? {
        plan.skipped.push(skipped);
        return Ok(plan);
    }
    if !path.exists() {
        return Ok(plan);
    }
    if contains_non_normal_component(&path) {
        plan.skipped.push(IndexProfileDeleteSkippedEntry {
            path,
            reason: "profile index path contains non-normal components".to_string(),
        });
        return Ok(plan);
    }
    if is_symlink(&path)? {
        plan.skipped.push(IndexProfileDeleteSkippedEntry {
            path,
            reason: "profile index path is a symlink; delete will not follow it".to_string(),
        });
        return Ok(plan);
    }
    let metadata = fs::metadata(&path)
        .with_context(|| format!("inspect profile index artifact: {}", path.display()))?;
    if !metadata.is_dir() {
        plan.skipped.push(IndexProfileDeleteSkippedEntry {
            path,
            reason: "profile index path is not a directory".to_string(),
        });
        return Ok(plan);
    }
    let approximate_bytes = approximate_dir_size(&path)?;
    plan.approximate_reclaim_bytes = approximate_bytes;
    plan.artifact = Some(IndexProfileArtifactPlan {
        path,
        approximate_bytes,
        reason: "profile-scoped published vector artifacts are obsolete".to_string(),
    });
    Ok(plan)
}

pub fn apply_index_profile_delete(
    data_dir: &Path,
    store: &Store,
    profile_id: &EmbeddingProfileId,
    active_profile_id: &EmbeddingProfileId,
    allow_active: bool,
) -> Result<(IndexProfileDeletePlan, IndexProfileDeleteApplyReport)> {
    let plan = plan_index_profile_delete(data_dir, store, profile_id, active_profile_id)?;
    if plan.active_profile && !allow_active {
        bail!(
            "refusing to delete active embedding profile {}; pass allow_active to clear active profile artifacts",
            profile_id
        );
    }

    let mut report = apply_index_profile_delete_artifacts(data_dir, profile_id, &plan)?;
    apply_index_profile_delete_sqlite(store, profile_id, &mut report)?;
    Ok((plan, report))
}

pub fn apply_index_profile_delete_artifacts(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
    plan: &IndexProfileDeletePlan,
) -> Result<IndexProfileDeleteApplyReport> {
    let mut report = IndexProfileDeleteApplyReport::default();
    if let Some(artifact) = &plan.artifact {
        remove_profile_artifact(data_dir, profile_id, artifact)?;
        report.reclaimed_bytes = artifact.approximate_bytes;
        report.removed_artifacts.push(artifact.clone());
    }
    Ok(report)
}

pub fn apply_index_profile_delete_sqlite(
    store: &Store,
    profile_id: &EmbeddingProfileId,
    report: &mut IndexProfileDeleteApplyReport,
) -> Result<()> {
    report.sqlite = store.delete_embedding_profile_index_data(profile_id)?;
    Ok(())
}

fn remove_profile_artifact(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
    artifact: &IndexProfileArtifactPlan,
) -> Result<()> {
    let expected = profile_index_root_dir(data_dir, profile_id);
    if absolute_lexical(&artifact.path)? != absolute_lexical(&expected)? {
        bail!(
            "profile delete artifact path does not match profile root: {}",
            artifact.path.display()
        );
    }
    validate_contained_path(&profile_indexes_root_dir(data_dir), &artifact.path)?;
    validate_profile_indexes_root(data_dir)?;
    validate_no_symlink_components(&profile_indexes_root_dir(data_dir), &artifact.path)?;
    match fs::remove_dir_all(&artifact.path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "remove profile index artifacts: {}",
                artifact.path.display()
            )
        }),
    }
}

fn index_root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes")
}

fn profile_indexes_root_dir(data_dir: &Path) -> PathBuf {
    index_root_dir(data_dir).join("profiles")
}

fn profile_index_root_dir(data_dir: &Path, profile_id: &EmbeddingProfileId) -> PathBuf {
    profile_indexes_root_dir(data_dir).join(profile_id.as_str())
}

fn unsafe_profile_indexes_root(data_dir: &Path) -> Result<Option<IndexProfileDeleteSkippedEntry>> {
    for path in [index_root_dir(data_dir), profile_indexes_root_dir(data_dir)] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Ok(Some(IndexProfileDeleteSkippedEntry {
                    path,
                    reason: "profile index root contains a symlink; delete will not follow it"
                        .to_string(),
                }));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Ok(Some(IndexProfileDeleteSkippedEntry {
                    path,
                    reason: "profile index root is not a directory".to_string(),
                }));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("inspect profile index root: {}", path.display()));
            }
        }
    }
    Ok(None)
}

fn validate_profile_indexes_root(data_dir: &Path) -> Result<()> {
    if let Some(skipped) = unsafe_profile_indexes_root(data_dir)? {
        bail!(
            "refusing to remove unsafe profile index root {}: {}",
            skipped.path.display(),
            skipped.reason
        );
    }
    Ok(())
}

fn validate_contained_path(root: &Path, candidate: &Path) -> Result<()> {
    let root = absolute_lexical(root)?;
    let candidate = absolute_lexical(candidate)?;
    if !candidate.starts_with(&root) {
        bail!(
            "refusing to remove profile index path outside profiles root: {}",
            candidate.display()
        );
    }
    Ok(())
}

fn validate_no_symlink_components(root: &Path, candidate: &Path) -> Result<()> {
    let root = absolute_lexical(root)?;
    let candidate = absolute_lexical(candidate)?;
    let relative = candidate.strip_prefix(&root).with_context(|| {
        format!(
            "profile index path is not inside profiles root: {}",
            candidate.display()
        )
    })?;
    let mut current = root;
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing to remove symlink profile index path component: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "inspect profile index path component: {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn contains_non_normal_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("inspect path: {}", path.display())),
    }
}

fn approximate_dir_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path).with_context(|| format!("read dir: {}", path.display()))? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(approximate_dir_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{EmbeddingCacheEntry, EmbeddingProfileConfig, SourceContentsReplacement};
    use crate::traits::VectorDocument;
    use crate::types::{
        Chunk, ChunkId, ChunkType, EvidenceId, EvidenceKind, EvidenceUnit, Source,
        SourceEmbeddingStatus, SourceId, SourceLocator, SourceStatus,
    };
    use tempfile::tempdir;

    fn profile(id: &str) -> EmbeddingProfileId {
        EmbeddingProfileId::new(id).unwrap()
    }

    fn config<'a>(model: &'a str) -> EmbeddingProfileConfig<'a> {
        crate::store::tests::test_profile_config("test", model, 2, true, "", "")
    }

    fn source() -> Source {
        Source {
            id: SourceId("src-1".to_string()),
            path: PathBuf::from("/tmp/doc.md"),
            hash: "hash-1".to_string(),
            status: SourceStatus::Indexed,
            parser_used: Some("test".to_string()),
            last_ingested_at: Some("2026-01-01".to_string()),
        }
    }

    fn evidence() -> EvidenceUnit {
        EvidenceUnit {
            id: EvidenceId("ev-1".to_string()),
            source_id: SourceId("src-1".to_string()),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: "doc.md".to_string(),
                line_start: 1,
                line_end: Some(1),
            },
            text: "alpha evidence".to_string(),
            text_hash: "ev-hash".to_string(),
            heading_path: vec!["H".to_string()],
            language: None,
            position: 0,
            annotations: Default::default(),
        }
    }

    fn child_chunk() -> Chunk {
        Chunk {
            id: ChunkId("chunk-1".to_string()),
            source_id: SourceId("src-1".to_string()),
            chunk_hash: "chunk-hash".to_string(),
            embedding_input_hash: Some("input-hash".to_string()),
            text: "alpha evidence".to_string(),
            context_text: Some("context".to_string()),
            token_count: 2,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["H".to_string()],
            evidence_unit_ids: vec![EvidenceId("ev-1".to_string())],
        }
    }

    fn seed_profile_data(store: &Store, profile_id: &EmbeddingProfileId) {
        store
            .ensure_embedding_profile(profile_id, config(profile_id.as_str()))
            .unwrap();
        let source = source();
        let evidence = evidence();
        let chunk = child_chunk();
        let vector = VectorDocument {
            chunk_id: chunk.id.clone(),
            source_id: source.id.clone(),
            vector: vec![0.1, 0.2],
        };
        store
            .replace_source_contents(SourceContentsReplacement {
                source: &source,
                evidence: &[evidence],
                chunks: std::slice::from_ref(&chunk),
                embedding_profile_id: profile_id,
                vectors: std::slice::from_ref(&vector),
                links: &[(chunk.id.clone(), EvidenceId("ev-1".to_string()))],
                evidence_spans: &[],
                image_artifacts: &[],
                graph_nodes: &[],
                graph_edges: &[],
            })
            .unwrap();
        store
            .upsert_embedding_cache_entries(
                profile_id,
                "config-hash",
                &[EmbeddingCacheEntry {
                    embedding_input_hash: "input-hash".to_string(),
                    vector: vec![0.1, 0.2],
                }],
            )
            .unwrap();
        store
            .set_source_embedding_status(
                profile_id,
                &SourceId("src-1".to_string()),
                SourceEmbeddingStatus::Embedded,
                1,
                None,
            )
            .unwrap();
        store
            .set_embedding_meta_for_profile(
                profile_id,
                &ChunkId("chunk-1".to_string()),
                0,
                profile_id.as_str(),
                "2026-01-01",
            )
            .unwrap();
    }

    fn write_artifact(data_dir: &Path, profile_id: &EmbeddingProfileId) -> PathBuf {
        let path = profile_index_root_dir(data_dir, profile_id).join("gen-1");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("vectors.hnsw"), b"12345").unwrap();
        fs::write(
            profile_index_root_dir(data_dir, profile_id).join("index-manifest.json"),
            b"{}",
        )
        .unwrap();
        profile_index_root_dir(data_dir, profile_id)
    }

    #[test]
    fn dry_run_plans_profile_sqlite_rows_and_artifacts_without_deleting() {
        let dir = tempdir().unwrap();
        let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
        let old = profile("old");
        seed_profile_data(&store, &old);
        let artifact = write_artifact(dir.path(), &old);

        let plan = plan_index_profile_delete(
            dir.path(),
            &store,
            &old,
            &EmbeddingProfileId::default_profile(),
        )
        .unwrap();

        assert_eq!(plan.profile_id, "old");
        assert!(!plan.active_profile);
        assert_eq!(plan.sqlite.chunk_vectors, 1);
        assert_eq!(plan.sqlite.embedding_cache_entries, 1);
        assert_eq!(plan.sqlite.source_embedding_statuses, 1);
        assert_eq!(plan.sqlite.embeddings_meta_entries, 1);
        assert_eq!(plan.sqlite.embedding_profile_index_meta_entries, 1);
        assert_eq!(plan.sqlite.embedding_profiles, 1);
        assert_eq!(plan.artifact.as_ref().unwrap().path, artifact);
        assert!(artifact.exists());
        assert_eq!(
            store.list_vector_documents_for_profile(&old).unwrap().len(),
            1
        );
    }

    #[test]
    fn confirmed_delete_removes_only_profile_scoped_data() {
        let dir = tempdir().unwrap();
        let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
        let old = profile("old");
        let kept = profile("kept");
        seed_profile_data(&store, &old);
        store
            .ensure_embedding_profile(&kept, config("kept"))
            .unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &kept,
                &[VectorDocument {
                    chunk_id: ChunkId("chunk-1".to_string()),
                    source_id: SourceId("src-1".to_string()),
                    vector: vec![0.3, 0.4],
                }],
            )
            .unwrap();
        let artifact = write_artifact(dir.path(), &old);

        let (_plan, report) = apply_index_profile_delete(
            dir.path(),
            &store,
            &old,
            &EmbeddingProfileId::default_profile(),
            false,
        )
        .unwrap();

        assert_eq!(report.sqlite.chunk_vectors, 1);
        assert_eq!(report.sqlite.embedding_profiles, 1);
        assert!(!artifact.exists());
        assert!(store
            .get_source(&SourceId("src-1".to_string()))
            .unwrap()
            .is_some());
        assert!(store
            .get_chunk(&ChunkId("chunk-1".to_string()))
            .unwrap()
            .is_some());
        assert!(store
            .list_vector_documents_for_profile(&old)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .list_vector_documents_for_profile(&kept)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn active_profile_delete_requires_explicit_allow_active() {
        let dir = tempdir().unwrap();
        let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
        let active = EmbeddingProfileId::default_profile();
        seed_profile_data(&store, &active);

        let err = apply_index_profile_delete(dir.path(), &store, &active, &active, false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("refusing to delete active embedding profile"));
        assert_eq!(
            store
                .list_vector_documents_for_profile(&active)
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_profile_artifact_is_skipped() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
        let old = profile("old");
        store.ensure_embedding_profile(&old, config("old")).unwrap();
        fs::create_dir_all(profile_indexes_root_dir(dir.path())).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, profile_index_root_dir(dir.path(), &old)).unwrap();

        let plan = plan_index_profile_delete(
            dir.path(),
            &store,
            &old,
            &EmbeddingProfileId::default_profile(),
        )
        .unwrap();

        assert!(plan.artifact.is_none());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("symlink"));
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_profile_indexes_root_is_skipped_without_following() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside_root = tempdir().unwrap();
        let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
        let old = profile("old");
        seed_profile_data(&store, &old);

        fs::create_dir_all(index_root_dir(dir.path())).unwrap();
        fs::create_dir_all(outside_root.path().join("old")).unwrap();
        fs::write(
            outside_root.path().join("old").join("vectors.hnsw"),
            b"12345",
        )
        .unwrap();
        symlink(outside_root.path(), profile_indexes_root_dir(dir.path())).unwrap();

        let plan = plan_index_profile_delete(
            dir.path(),
            &store,
            &old,
            &EmbeddingProfileId::default_profile(),
        )
        .unwrap();

        assert!(plan.artifact.is_none());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("symlink"));

        let (_plan, report) = apply_index_profile_delete(
            dir.path(),
            &store,
            &old,
            &EmbeddingProfileId::default_profile(),
            false,
        )
        .unwrap();

        assert_eq!(report.sqlite.chunk_vectors, 1);
        assert!(outside_root
            .path()
            .join("old")
            .join("vectors.hnsw")
            .exists());
    }
}
