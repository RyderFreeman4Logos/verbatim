use super::*;
use crate::collection::{CollectionMemberCandidate, CollectionSyncPathInput, CollectionSyncReport};
use crate::config::{ChatVisionAttachmentConfig, RetrievalConfig};
use crate::generate::Generator;
use crate::provider::{ChatModel, ChatRequest, ChatResponse, ChatStream, ProviderResult};
use crate::retrieve::RetrievalPipeline;
use crate::types::{CitationRef, GraphNodeKind};
use async_trait::async_trait;
use futures::StreamExt;
use rusqlite::{params, Connection};

#[derive(Clone)]
struct RelocationEmbeddingClient;

#[async_trait]
impl EmbeddingClient for RelocationEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

struct CitationChatModel;

#[async_trait]
impl ChatModel for CitationChatModel {
    async fn chat(&self, _request: ChatRequest) -> ProviderResult<ChatResponse> {
        Ok(ChatResponse {
            content: "The relocation evidence remains available [E1].".into(),
            finish_reason: None,
            usage: None,
        })
    }

    async fn stream_chat(&self, _request: ChatRequest) -> ProviderResult<ChatStream> {
        Ok(futures::stream::empty().boxed())
    }
}

#[derive(Debug, PartialEq)]
struct SourceSnapshot {
    id: SourceId,
    path: PathBuf,
    hash: String,
    status: SourceStatus,
    parser_used: Option<String>,
    last_ingested_at: Option<String>,
}

impl From<Source> for SourceSnapshot {
    fn from(source: Source) -> Self {
        Self {
            id: source.id,
            path: source.path,
            hash: source.hash,
            status: source.status,
            parser_used: source.parser_used,
            last_ingested_at: source.last_ingested_at,
        }
    }
}

#[derive(Debug, PartialEq)]
struct CatalogSnapshot {
    source_count: usize,
    source: SourceSnapshot,
    evidence: Vec<EvidenceUnit>,
    chunks: serde_json::Value,
    graph_nodes: Vec<GraphNode>,
    graph_edges: Vec<GraphEdge>,
    spans: serde_json::Value,
    vector_count: usize,
    generations: Vec<crate::store::EmbeddingProfileIndexGeneration>,
}

#[derive(Debug, PartialEq)]
struct CitationSnapshot {
    chunk_ids: Vec<ChunkId>,
    evidence_ids: Vec<EvidenceId>,
    citation_source_id: SourceId,
    citation_evidence_id: EvidenceId,
    citation_locator: SourceLocator,
}

async fn indexed_fixture(
    root: &Path,
    name: &str,
    on_disk: bool,
) -> (IngestPipeline<RelocationEmbeddingClient>, SourceId, PathBuf) {
    let path = root.join(format!("{name}.txt"));
    let body = format!(
        "relocationneedle {name} {}",
        (0..700)
            .map(|index| format!("stable{index}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    fs::write(&path, body).unwrap();
    let store = if on_disk {
        Store::new(&root.join(format!("{name}.db"))).unwrap()
    } else {
        Store::in_memory().unwrap()
    };
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        RelocationEmbeddingClient,
        root.to_path_buf(),
    );
    let source_id = pipeline.add_source(&path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    (pipeline, source_id, path)
}

fn catalog_snapshot(
    pipeline: &IngestPipeline<RelocationEmbeddingClient>,
    source_id: &SourceId,
) -> CatalogSnapshot {
    let store = pipeline.store();
    let chunks = store.list_chunks_by_source(source_id).unwrap();
    let spans = chunks
        .iter()
        .map(|chunk| {
            (
                chunk.id.clone(),
                store.list_chunk_evidence_spans(&chunk.id).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    CatalogSnapshot {
        source_count: store.list_sources().unwrap().len(),
        source: store.get_source(source_id).unwrap().unwrap().into(),
        evidence: store.list_evidence_by_source(source_id).unwrap(),
        chunks: serde_json::to_value(chunks).unwrap(),
        graph_nodes: store.list_graph_nodes_by_source(source_id).unwrap(),
        graph_edges: store.list_graph_edges_by_source(source_id).unwrap(),
        spans: serde_json::to_value(spans).unwrap(),
        vector_count: store
            .count_vector_documents_for_profile(
                &EmbeddingProfileId::default_profile(),
                Some(source_id),
            )
            .unwrap(),
        generations: store.profile_index_generations().unwrap(),
    }
}

async fn retrieval_citation_snapshot(
    pipeline: &IngestPipeline<RelocationEmbeddingClient>,
    source_id: &SourceId,
) -> CitationSnapshot {
    let lexical_index = pipeline.lexical_index();
    let retrieval_config = RetrievalConfig {
        dense_top_k: 0,
        bm25_top_k: 8,
        ..RetrievalConfig::default()
    };
    let results = RetrievalPipeline::new(
        pipeline.vector_index(),
        &lexical_index,
        pipeline.store(),
        &RelocationEmbeddingClient,
        &retrieval_config,
    )
    .with_embedding_enabled(false)
    .search_filtered("relocationneedle", Some(source_id))
    .await
    .unwrap();
    assert!(!results.is_empty());
    let generator = Generator::with_chat_model(
        Arc::new(CitationChatModel),
        false,
        ChatVisionAttachmentConfig::default(),
    );
    let generated = generator
        .generate("Is relocation evidence available?", &results)
        .await
        .unwrap();
    let CitationRef {
        source_id: citation_source_id,
        evidence_id: citation_evidence_id,
        locator: citation_locator,
        ..
    } = generated.citations.into_iter().next().unwrap();
    CitationSnapshot {
        chunk_ids: results
            .iter()
            .map(|result| result.chunk_id.clone())
            .collect(),
        evidence_ids: results
            .iter()
            .flat_map(|result| result.evidence_units.iter().map(|unit| unit.id.clone()))
            .collect(),
        citation_source_id,
        citation_evidence_id,
        citation_locator,
    }
}

fn assert_locator_path(locator: &SourceLocator, expected: &Path) {
    let expected = expected.to_str().unwrap();
    match locator {
        SourceLocator::Document { path_or_url, .. } => assert_eq!(path_or_url, expected),
        SourceLocator::Markdown { path, .. } => assert_eq!(path, expected),
        other => panic!("expected path-bearing locator, got {other:?}"),
    }
}

fn identity_accounting(
    snapshot: &CatalogSnapshot,
) -> (
    Vec<(EvidenceId, SourceId, Option<EvidenceId>)>,
    serde_json::Value,
    Vec<(GraphNodeId, SourceId, String, String)>,
    Vec<(GraphEdgeId, SourceId, GraphNodeId, GraphNodeId)>,
) {
    let evidence = snapshot
        .evidence
        .iter()
        .map(|unit| {
            (
                unit.id.clone(),
                unit.source_id.clone(),
                unit.derived_from.clone(),
            )
        })
        .collect();
    let nodes = snapshot
        .graph_nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                node.source_id.clone(),
                node.kind.as_str().to_string(),
                node.external_id.clone(),
            )
        })
        .collect();
    let edges = snapshot
        .graph_edges
        .iter()
        .map(|edge| {
            (
                edge.id.clone(),
                edge.source_id.clone(),
                edge.from_node_id.clone(),
                edge.to_node_id.clone(),
            )
        })
        .collect();
    (evidence, snapshot.chunks.clone(), nodes, edges)
}

#[tokio::test]
async fn issue_332_explicit_move_preserves_identity_and_retrieval() {
    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(tempdir.path(), "old", false).await;
    let new_path = tempdir.path().join("new.txt");
    let before = catalog_snapshot(&pipeline, &source_id);
    let before_identity = identity_accounting(&before);
    let before_citation = retrieval_citation_snapshot(&pipeline, &source_id).await;
    assert!(before
        .chunks
        .as_array()
        .unwrap()
        .iter()
        .any(|chunk| chunk["chunk_type"] == "Child"));
    assert!(before
        .chunks
        .as_array()
        .unwrap()
        .iter()
        .any(|chunk| chunk["chunk_type"] == "Parent"));
    fs::rename(&old_path, &new_path).unwrap();

    let relocated = pipeline.relocate_source(&source_id, &new_path).unwrap();

    assert_eq!(relocated.id, source_id);
    assert_eq!(relocated.path, fs::canonicalize(&new_path).unwrap());
    assert!(!old_path.exists());
    let after_relocation = catalog_snapshot(&pipeline, &source_id);
    assert_eq!(after_relocation.source_count, 1);
    assert_eq!(after_relocation.source.id, before.source.id);
    assert_eq!(after_relocation.source.hash, before.source.hash);
    assert_eq!(after_relocation.source.status, before.source.status);
    assert_eq!(
        after_relocation.source.parser_used,
        before.source.parser_used
    );
    assert_eq!(
        after_relocation.source.last_ingested_at,
        before.source.last_ingested_at
    );
    assert_eq!(identity_accounting(&after_relocation), before_identity);
    assert_eq!(after_relocation.vector_count, before.vector_count);
    assert_eq!(after_relocation.generations, before.generations);
    for unit in &after_relocation.evidence {
        assert_locator_path(&unit.locator, &relocated.path);
    }
    for chunk in pipeline.store().list_chunks_by_source(&source_id).unwrap() {
        for span in pipeline
            .store()
            .list_chunk_evidence_spans(&chunk.id)
            .unwrap()
        {
            assert_locator_path(&span.locator, &relocated.path);
        }
    }
    for node in &after_relocation.graph_nodes {
        if node.kind == GraphNodeKind::EvidenceUnit {
            assert_locator_path(node.locator.as_ref().unwrap(), &relocated.path);
        }
        if node.kind == GraphNodeKind::Source {
            assert_eq!(node.label.as_deref(), Some("new.txt"));
            assert_eq!(
                node.metadata.as_ref().unwrap()["path"],
                relocated.path.to_str().unwrap()
            );
        }
    }

    let no_op = pipeline.ingest_source(&source_id).await.unwrap();
    assert_eq!(no_op.changed_chunks, 0);
    let after_ingest = catalog_snapshot(&pipeline, &source_id);
    assert_eq!(identity_accounting(&after_ingest), before_identity);
    assert_eq!(after_ingest.vector_count, before.vector_count);
    assert_eq!(after_ingest.generations, before.generations);
    let after_citation = retrieval_citation_snapshot(&pipeline, &source_id).await;
    assert_eq!(after_citation.chunk_ids, before_citation.chunk_ids);
    assert_eq!(after_citation.evidence_ids, before_citation.evidence_ids);
    assert_eq!(
        after_citation.citation_source_id,
        before_citation.citation_source_id
    );
    assert_eq!(
        after_citation.citation_evidence_id,
        before_citation.citation_evidence_id
    );
    assert_locator_path(&after_citation.citation_locator, &relocated.path);
}

fn synthetic_evidence(id: &str, source_id: &SourceId, position: u32) -> EvidenceUnit {
    EvidenceUnit {
        id: EvidenceId(id.into()),
        source_id: source_id.clone(),
        kind: EvidenceKind::Text,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: "/tmp/parser.txt".into(),
            line_start: position + 1,
            line_end: None,
        },
        text: format!("evidence {position}"),
        text_hash: format!("hash-{position}"),
        heading_path: Vec::new(),
        position,
    }
}

#[test]
fn issue_332_parser_identity_remap_is_complete_and_fail_closed() {
    let parser_source = SourceId("parser-source".into());
    let catalog_source = SourceId("catalog-source".into());
    let first_id = EvidenceId("parser-source:first".into());
    let first = synthetic_evidence(&first_id.0, &parser_source, 0);
    let mut second = synthetic_evidence("parser-source:second", &parser_source, 1);
    second.derived_from = Some(first_id);

    let remapped = remap_parser_evidence_identity(
        vec![first.clone(), second.clone()],
        &parser_source,
        &catalog_source,
    )
    .unwrap();
    assert_eq!(remapped[0].id, EvidenceId("catalog-source:first".into()));
    assert_eq!(remapped[1].id, EvidenceId("catalog-source:second".into()));
    assert_eq!(remapped[0].source_id, catalog_source);
    assert_eq!(remapped[1].source_id, catalog_source);
    assert_eq!(
        remapped[1].derived_from,
        Some(EvidenceId("catalog-source:first".into()))
    );
    assert_eq!(
        remap_parser_evidence_identity(vec![first.clone()], &parser_source, &parser_source)
            .unwrap()[0]
            .id,
        first.id
    );

    let mut mixed = first.clone();
    mixed.source_id = SourceId("other-source".into());
    assert!(remap_parser_evidence_identity(vec![mixed], &parser_source, &catalog_source).is_err());
    for bad_id in ["", "not-prefixed", "parser-source-without-boundary"] {
        let bad = synthetic_evidence(bad_id, &parser_source, 0);
        assert!(
            remap_parser_evidence_identity(vec![bad], &parser_source, &catalog_source).is_err()
        );
    }
    assert!(remap_parser_evidence_identity(
        vec![first.clone(), first.clone()],
        &parser_source,
        &catalog_source,
    )
    .is_err());
    let mut dangling = second;
    dangling.derived_from = Some(EvidenceId("parser-source:missing".into()));
    assert!(
        remap_parser_evidence_identity(vec![first, dangling], &parser_source, &catalog_source,)
            .is_err()
    );
}

#[tokio::test]
async fn issue_332_relocation_rolls_back_then_recovers() {
    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "rollback", true).await;
    let new_path = tempdir.path().join("recovered.txt");
    fs::rename(&old_path, &new_path).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);
    pipeline
        .store()
        .connection()
        .execute_batch(
            "CREATE TRIGGER issue_332_relocation_failpoint
             BEFORE UPDATE OF locator_json ON chunk_evidence_spans
             BEGIN
                 SELECT RAISE(ABORT, 'issue-332 relocation failpoint');
             END;",
        )
        .unwrap();

    let error = pipeline.relocate_source(&source_id, &new_path).unwrap_err();

    assert!(format!("{error:#}").contains("issue-332 relocation failpoint"));
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
    pipeline
        .store()
        .connection()
        .execute_batch("DROP TRIGGER issue_332_relocation_failpoint;")
        .unwrap();
    let relocated = pipeline.relocate_source(&source_id, &new_path).unwrap();
    assert_eq!(relocated.path, fs::canonicalize(&new_path).unwrap());
    assert_eq!(relocated.id, source_id);
}

async fn assert_failed_relocation_preserves_snapshot(
    pipeline: &mut IngestPipeline<RelocationEmbeddingClient>,
    source_id: &SourceId,
    target: &Path,
) {
    let before = catalog_snapshot(pipeline, source_id);
    let before_citation = retrieval_citation_snapshot(pipeline, source_id).await;
    assert!(pipeline.relocate_source(source_id, target).is_err());
    assert_eq!(catalog_snapshot(pipeline, source_id), before);
    assert_eq!(
        retrieval_citation_snapshot(pipeline, source_id).await,
        before_citation
    );
}

#[tokio::test]
async fn issue_332_relocation_failures_preserve_original_snapshot() {
    let tempdir = tempfile::tempdir().unwrap();

    let missing_dir = tempdir.path().join("missing");
    fs::create_dir(&missing_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&missing_dir, "old", false).await;
    let missing_target = missing_dir.join("missing.txt");
    fs::remove_file(&old_path).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &missing_target).await;

    let unsupported_dir = tempdir.path().join("unsupported");
    fs::create_dir(&unsupported_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&unsupported_dir, "old", false).await;
    let unsupported_target = unsupported_dir.join("new.bin");
    fs::rename(&old_path, &unsupported_target).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &unsupported_target)
        .await;

    let parser_dir = tempdir.path().join("parser");
    fs::create_dir(&parser_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&parser_dir, "old", false).await;
    let parser_target = parser_dir.join("new.md");
    fs::rename(&old_path, &parser_target).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &parser_target).await;

    let changed_dir = tempdir.path().join("changed");
    fs::create_dir(&changed_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&changed_dir, "old", false).await;
    let changed_target = changed_dir.join("new.txt");
    fs::rename(&old_path, &changed_target).unwrap();
    fs::write(&changed_target, "changed bytes").unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &changed_target).await;

    let conflict_dir = tempdir.path().join("conflict");
    fs::create_dir(&conflict_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&conflict_dir, "old", false).await;
    let conflict_target = conflict_dir.join("owner.txt");
    fs::write(&conflict_target, "relocationneedle conflict owner").unwrap();
    let conflict_id = pipeline.add_source(&conflict_target).unwrap();
    pipeline.ingest_source(&conflict_id).await.unwrap();
    fs::remove_file(&old_path).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &conflict_target).await;
    assert_ne!(source_id, conflict_id);
    assert_eq!(pipeline.store().list_sources().unwrap().len(), 2);

    let old_present_dir = tempdir.path().join("old-present");
    fs::create_dir(&old_present_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&old_present_dir, "old", false).await;
    let old_present_target = old_present_dir.join("new.txt");
    fs::copy(&old_path, &old_present_target).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &old_present_target)
        .await;

    let collection_dir = tempdir.path().join("collection");
    fs::create_dir(&collection_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&collection_dir, "old", false).await;
    pipeline.store().create_collection("docs", &[]).unwrap();
    pipeline
        .store()
        .replace_collection_members(
            "docs",
            &[CollectionMemberCandidate {
                source_id: source_id.clone(),
                logical_path: "old.txt".into(),
                source_path: old_path.clone(),
            }],
            CollectionSyncReport {
                member_count: 1,
                added: 1,
                removed: 0,
                unchanged: 0,
                scanned_roots: 1,
                max_depth: 1,
                skipped: Vec::new(),
            },
        )
        .unwrap();
    let collection_target = collection_dir.join("new.txt");
    fs::rename(&old_path, &collection_target).unwrap();
    assert_failed_relocation_preserves_snapshot(&mut pipeline, &source_id, &collection_target)
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn issue_332_relocation_rejects_final_component_symlink() {
    use std::os::unix::fs::symlink;

    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "symlink", false).await;
    let real_target = tempdir.path().join("real.txt");
    let symlink_target = tempdir.path().join("link.txt");
    fs::rename(&old_path, &real_target).unwrap();
    symlink(&real_target, &symlink_target).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);

    let error = pipeline
        .relocate_source(&source_id, &symlink_target)
        .unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert!(format!("{error:#}").contains("must not be a symlink"));
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[cfg(unix)]
#[tokio::test]
async fn issue_332_relocation_parses_held_snapshot_across_path_aba() {
    use std::os::unix::fs::PermissionsExt;

    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "parse-aba", false).await;
    let target = tempdir.path().join("target.txt");
    let held_backup = tempdir.path().join("held-backup.txt");
    let replacement = tempdir.path().join("replacement.txt");
    fs::rename(&old_path, &target).unwrap();
    let original_bytes = fs::read(&target).unwrap();
    let mut replacement_bytes = original_bytes.clone();
    replacement_bytes.extend_from_slice(b"\r\n");
    assert_ne!(replacement_bytes, original_bytes);
    fs::write(&replacement, replacement_bytes).unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o000)).unwrap();

    let target_before_parse = target.clone();
    let replacement_target = target.clone();
    let target_after_parse = target.clone();
    let backup_before_parse = held_backup.clone();
    let backup_after_parse = held_backup.clone();
    pipeline.store().set_source_relocation_parse_hooks(
        move || {
            fs::rename(target_before_parse, backup_before_parse).unwrap();
            fs::rename(replacement, replacement_target).unwrap();
        },
        move || {
            fs::remove_file(&target_after_parse).unwrap();
            fs::rename(backup_after_parse, target_after_parse).unwrap();
        },
    );

    let relocated = pipeline.relocate_source(&source_id, &target).unwrap();

    assert_eq!(relocated.path, fs::canonicalize(&target).unwrap());
    assert_eq!(fs::read(&target).unwrap(), original_bytes);
    for evidence in pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
    {
        assert_locator_path(&evidence.locator, &relocated.path);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn issue_332_relocation_revalidates_target_inside_transaction() {
    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "replace", false).await;
    let target = tempdir.path().join("target.txt");
    let replacement = tempdir.path().join("replacement.txt");
    fs::rename(&old_path, &target).unwrap();
    fs::write(
        &replacement,
        "replacement bytes accepted by no prior validation",
    )
    .unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);
    let target_for_hook = target.clone();
    pipeline
        .store()
        .set_source_relocation_before_mutation_hook(move || {
            fs::rename(replacement, target_for_hook).unwrap();
        });

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert!(format!("{error:#}").contains("no longer identifies the held snapshot"));
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[tokio::test]
async fn issue_332_relocation_revalidates_old_path_inside_transaction() {
    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "reappear", false).await;
    let target = tempdir.path().join("target.txt");
    fs::rename(&old_path, &target).unwrap();
    let original_bytes = fs::read(&target).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);
    let old_path_for_hook = old_path.clone();
    pipeline
        .store()
        .set_source_relocation_before_mutation_hook(move || {
            fs::write(old_path_for_hook, original_bytes).unwrap();
        });

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert!(format!("{error:#}").contains("reappeared before relocation commit"));
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[tokio::test]
async fn issue_332_source_path_uniqueness_fails_closed() {
    let tempdir = tempfile::tempdir().unwrap();
    let legacy_path = tempdir.path().join("legacy.db");
    let duplicate_path = tempdir.path().join("same.txt");
    let connection = Connection::open(&legacy_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sources (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                hash TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Pending',
                parser_used TEXT,
                last_ingested_at TEXT
             );",
        )
        .unwrap();
    let duplicate_text = duplicate_path.to_str().unwrap();
    connection
        .execute(
            "INSERT INTO sources (id, path, hash, status) VALUES (?1, ?2, 'hash-a', 'Indexed')",
            params!["legacy-a", duplicate_text],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sources (id, path, hash, status) VALUES (?1, ?2, 'hash-b', 'Indexed')",
            params!["legacy-b", duplicate_text],
        )
        .unwrap();
    drop(connection);

    let error = match Store::new(&legacy_path) {
        Ok(_) => panic!("legacy duplicate source paths must block migration"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(message.contains("legacy duplicate source locations require manual resolution"));
    assert!(message.contains(duplicate_text));
    let preserved = Connection::open(&legacy_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sources", [], |row| {
            row.get::<_, usize>(0)
        })
        .unwrap();
    assert_eq!(preserved, 2);

    let valid_dir = tempdir.path().join("valid");
    fs::create_dir(&valid_dir).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&valid_dir, "old", false).await;
    let new_path = valid_dir.join("new.txt");
    fs::rename(&old_path, &new_path).unwrap();
    pipeline.relocate_source(&source_id, &new_path).unwrap();
    assert_eq!(pipeline.add_source(&new_path).unwrap(), source_id);
    assert_eq!(pipeline.store().list_sources().unwrap().len(), 1);

    fs::write(&old_path, "unrelated bytes at the reused old path").unwrap();
    let direct_error = pipeline.add_source(&old_path).unwrap_err();
    assert!(format!("{direct_error:#}").contains("source identity conflict"));
    assert_eq!(pipeline.store().list_sources().unwrap().len(), 1);

    pipeline.store().create_collection("reused", &[]).unwrap();
    let sync_error = pipeline
        .sync_collection(
            "reused",
            &[CollectionSyncPathInput {
                path: old_path,
                logical_path: None,
            }],
            None,
        )
        .unwrap_err();
    assert!(format!("{sync_error:#}").contains("source identity conflict"));
    assert!(pipeline
        .store()
        .list_collection_members("reused")
        .unwrap()
        .is_empty());
}
