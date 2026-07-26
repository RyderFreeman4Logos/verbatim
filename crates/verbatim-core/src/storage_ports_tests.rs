//! Contract tests for narrow storage ports (DIST-004 / issue #350).

#[path = "storage_ports_test_stubs.rs"]
mod stubs;
use stubs::*;

use super::*;
use crate::auth::{Principal, Role};
use crate::task::{TaskId, TaskKind, TaskStatus, TaskSummary};
use crate::types::{EmbeddingProfileId, GraphNodeId, SourceId};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared helpers / validation
// ---------------------------------------------------------------------------

#[test]
fn page_request_rejects_zero_limit() {
    assert!(PageRequest::new(0).is_err());
    let page = PageRequest::new(10).unwrap();
    assert_eq!(page.limit, 10);
    assert!(page.cursor.is_none());
}

#[test]
fn page_cursor_rejects_empty() {
    assert!(PageCursor::new("").is_err());
    assert!(PageCursor::new("   ").is_err());
    assert_eq!(PageCursor::new("abc").unwrap().0, "abc");
}

#[test]
fn auth_context_from_principal_and_validation() {
    let local = StorageAuthContext::from_principal(&Principal::LocalAnonymous);
    assert_eq!(local.principal, StoragePrincipal::LocalAnonymous);
    local.validate().unwrap();

    let token = StorageAuthContext::from_principal(&Principal::Token { role: Role::Reader })
        .with_acl_scope("col/alpha")
        .with_request_id("req-1");
    token.validate().unwrap();
    assert!(matches!(
        token.principal,
        StoragePrincipal::Token { ref role } if role == "reader"
    ));

    let mut bad = token.clone();
    bad.acl_scope = Some("  ".into());
    assert!(bad.validate().is_err());

    bad = token;
    bad.schema_version = 99;
    assert!(bad.validate().is_err());
}

#[test]
fn storage_error_class_names_cover_all_variants() {
    let cases = [
        (
            StorageError::timeout("search"),
            "timeout",
            "storage timeout during search",
        ),
        (
            StorageError::conflict("source"),
            "conflict",
            "storage conflict on source",
        ),
        (
            StorageError::stale_generation(StorageGeneration::new(1), StorageGeneration::new(3)),
            "stale_generation",
            "stale storage generation: expected 1, actual 3",
        ),
        (
            StorageError::unsupported(StorageCapabilityKind::GraphSearch, "neighbors"),
            "unsupported",
            "unsupported storage capability GraphSearch for operation neighbors",
        ),
        (
            StorageError::not_found("blob", "b1"),
            "not_found",
            "storage resource blob not found: b1",
        ),
        (
            StorageError::unauthorized("reader cannot write"),
            "unauthorized",
            "storage unauthorized: reader cannot write",
        ),
        (
            StorageError::invalid_request("empty query"),
            "invalid_request",
            "invalid storage request: empty query",
        ),
        (
            StorageError::unavailable("restarting"),
            "unavailable",
            "storage unavailable: restarting",
        ),
    ];
    for (err, class, prefix) in cases {
        assert_eq!(err.class_name(), class);
        assert!(err.to_string().starts_with(prefix), "{}", err);
        let json = serde_json::to_string(&err).unwrap();
        let round: StorageError = serde_json::from_str(&json).unwrap();
        assert_eq!(round.class_name(), class);
    }
}

#[test]
fn capability_kind_lifecycle_classification() {
    for kind in StorageCapabilityKind::ALL {
        assert!(!kind.as_str().is_empty());
        assert_eq!(kind.is_authoritative(), !kind.is_derived());
    }
    assert!(StorageCapabilityKind::CatalogStore.is_authoritative());
    assert!(StorageCapabilityKind::EvidenceStore.is_authoritative());
    assert!(StorageCapabilityKind::BlobStore.is_authoritative());
    assert!(StorageCapabilityKind::TaskQueue.is_authoritative());
    assert!(StorageCapabilityKind::LexicalSearch.is_derived());
    assert!(StorageCapabilityKind::VectorSearch.is_derived());
    assert!(StorageCapabilityKind::GraphSearch.is_derived());
    assert!(StorageCapabilityKind::IndexPublisher.is_derived());
}

#[test]
fn capability_descriptor_discovery_and_require() {
    let stub = StubStorage::empty();
    assert!(!stub.supports(StorageCapabilityKind::CatalogStore));
    let err = stub
        .require(StorageCapabilityKind::CatalogStore, "list_sources")
        .unwrap_err();
    assert_eq!(err.class_name(), "unsupported");

    let full = StubStorage::with_all_capabilities();
    for kind in StorageCapabilityKind::ALL {
        assert!(full.supports(kind));
        full.require(kind, "op").unwrap();
    }
    let desc = full.capability_descriptor();
    desc.validate().unwrap();
    assert_eq!(desc.backend_label.as_deref(), Some("test_stub"));
    assert_eq!(desc.capabilities.len(), 8);
}

#[test]
fn unknown_schema_versions_fail_closed_on_decode() {
    let mut auth = auth();
    auth.schema_version = 7;
    let bytes = serde_json::to_vec(&auth).unwrap();
    let err = decode_auth_context_json(&bytes).unwrap_err();
    assert_eq!(err.class_name(), "invalid_request");
    assert!(err.to_string().contains("schema version"));

    let mut desc = StorageCapabilityDescriptor::new([StorageCapabilityKind::BlobStore]);
    desc.schema_version = 3;
    let bytes = serde_json::to_vec(&desc).unwrap();
    assert!(decode_capability_descriptor_json(&bytes).is_err());

    let mut manifest = PublicationManifest::new(
        StorageGeneration::new(2),
        "deadbeef",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    manifest.schema_version = 9;
    let bytes = serde_json::to_vec(&manifest).unwrap();
    assert!(decode_publication_manifest_json(&bytes).is_err());
}

#[test]
fn publication_manifest_validation() {
    assert!(PublicationManifest::new(StorageGeneration::INITIAL, "", "t").is_err());
    assert!(PublicationManifest::new(StorageGeneration::INITIAL, "c", "").is_err());
    let m = PublicationManifest::new(StorageGeneration::new(4), "abc", "ts")
        .unwrap()
        .with_profile(EmbeddingProfileId::default_profile())
        .with_components(["lexical".into(), "vector".into()]);
    m.validate().unwrap();
    assert_eq!(m.components.len(), 2);
    let bytes = serde_json::to_vec(&m).unwrap();
    let round = decode_publication_manifest_json(&bytes).unwrap();
    assert_eq!(round.generation, StorageGeneration::new(4));
}

#[test]
fn duration_millis_roundtrip() {
    let d = DurationMillis::from_duration(Duration::from_secs(2));
    assert_eq!(d.0, 2000);
    assert_eq!(d.as_duration(), Duration::from_millis(2000));
}

#[test]
fn blob_id_rejects_empty() {
    assert!(BlobId::new("").is_err());
    assert_eq!(BlobId::new("sha256:aa").unwrap().0, "sha256:aa");
}

// ---------------------------------------------------------------------------
// Trait compliance via stubs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_store_stub_list_and_not_found() {
    let stub = StubStorage::with_all_capabilities();
    let page = PageRequest::new(10).unwrap();
    let listed = stub
        .list_sources(CatalogListSourcesRequest {
            auth: auth(),
            page: page.clone(),
        })
        .await
        .unwrap();
    assert!(listed.page.items.is_empty());

    let err = stub
        .get_source(CatalogGetSourceRequest {
            auth: auth(),
            source_id: SourceId("missing".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.class_name(), "not_found");
}

#[tokio::test]
async fn evidence_and_blob_and_task_stubs_are_callable() {
    let stub = StubStorage::with_all_capabilities();
    let page = PageRequest::new(5).unwrap();

    stub.list_evidence(EvidenceListRequest {
        auth: auth(),
        filter: EvidenceFilter {
            source_id: None,
            evidence_id: None,
            chunk_id: None,
        },
        page: page.clone(),
    })
    .await
    .unwrap();

    let put = stub
        .put_blob(BlobPutRequest {
            auth: auth(),
            blob_id: BlobId::new("b1").unwrap(),
            content_type: "image/png".into(),
            bytes: vec![1, 2, 3],
            image_id: None,
            evidence_id: None,
        })
        .await
        .unwrap();
    assert_eq!(put.byte_len, 3);

    let enq = stub
        .enqueue(TaskEnqueueRequest {
            auth: auth(),
            kind: TaskKind::Ingest,
            request: serde_json::json!({"source_id": "s1"}),
        })
        .await
        .unwrap();
    assert_eq!(enq.task_id.0, "task-1");
    assert_eq!(enq.status, TaskStatus::Queued);
}

#[tokio::test]
async fn search_stubs_return_empty_pages_with_generation() {
    let stub = StubStorage::with_all_capabilities();
    let page = PageRequest::new(3).unwrap();

    let lex = LexicalSearch::search(
        &stub,
        LexicalSearchRequest {
            auth: auth(),
            query: "hello".into(),
            page: page.clone(),
            source_filter: None,
            collection_filter: None,
            min_generation: None,
        },
    )
    .await
    .unwrap();
    assert!(lex.page.items.is_empty());
    assert_eq!(lex.generation, StorageGeneration::INITIAL);

    let vec = VectorSearch::search(
        &stub,
        VectorSearchRequest {
            auth: auth(),
            query_vector: vec![0.1, 0.2],
            page,
            profile_id: EmbeddingProfileId::default_profile(),
            source_filter: None,
            collection_filter: None,
            min_generation: Some(StorageGeneration::new(0)),
        },
    )
    .await
    .unwrap();
    assert!(vec.page.items.is_empty());
}

#[tokio::test]
async fn graph_search_stub_not_found_and_neighbors() {
    let stub = StubStorage::with_all_capabilities();
    let err = stub
        .get_node(GraphGetNodeRequest {
            auth: auth(),
            node_id: GraphNodeId("n1".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.class_name(), "not_found");

    let neighbors = stub
        .neighbors(GraphNeighborsRequest {
            auth: auth(),
            node_id: GraphNodeId("n1".into()),
            page: PageRequest::new(10).unwrap(),
            edge_types: vec!["contains".into()],
        })
        .await
        .unwrap();
    assert!(neighbors.page.items.is_empty());
}

#[tokio::test]
async fn index_publisher_cas_and_stale_generation() {
    let stub = StubStorage::with_all_capabilities();
    let manifest =
        PublicationManifest::new(StorageGeneration::new(1), "checksum-1", "ts-1").unwrap();
    let published = stub
        .publish(IndexPublishRequest {
            auth: auth(),
            manifest: manifest.clone(),
            expected_current: Some(StorageGeneration::INITIAL),
        })
        .await
        .unwrap();
    assert_eq!(published.generation, StorageGeneration::new(1));

    let current = stub
        .current(IndexCurrentRequest {
            auth: auth(),
            profile_id: None,
        })
        .await
        .unwrap();
    assert_eq!(current.generation, StorageGeneration::new(1));
    assert_eq!(current.manifest.as_ref().unwrap().checksum, "checksum-1");

    let stale = stub
        .publish(IndexPublishRequest {
            auth: auth(),
            manifest: PublicationManifest::new(StorageGeneration::new(2), "c2", "ts-2").unwrap(),
            expected_current: Some(StorageGeneration::INITIAL),
        })
        .await
        .unwrap_err();
    assert_eq!(stale.class_name(), "stale_generation");
}

#[tokio::test]
async fn fault_injection_returns_typed_timeout_conflict_unsupported() {
    let stub = StubStorage::with_all_capabilities();
    stub.force(StorageError::timeout("lexical_search"));
    let err = LexicalSearch::search(
        &stub,
        LexicalSearchRequest {
            auth: auth(),
            query: "q".into(),
            page: PageRequest::new(1).unwrap(),
            source_filter: None,
            collection_filter: None,
            min_generation: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.class_name(), "timeout");

    stub.force(StorageError::conflict("collection"));
    let err = stub
        .create_collection(CatalogCreateCollectionRequest {
            auth: auth(),
            name: "c".into(),
            ignore_patterns: Vec::new(),
            watch_enabled: false,
            auto_index_enabled: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.class_name(), "conflict");

    let empty = StubStorage::empty();
    let err = VectorSearch::search(
        &empty,
        VectorSearchRequest {
            auth: auth(),
            query_vector: vec![1.0],
            page: PageRequest::new(1).unwrap(),
            profile_id: EmbeddingProfileId::default_profile(),
            source_filter: None,
            collection_filter: None,
            min_generation: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.class_name(), "unsupported");
}

#[tokio::test]
async fn unsupported_capability_does_not_leak_backend_types() {
    let empty = StubStorage::empty();
    let err = empty
        .list_tasks(TaskListRequest {
            auth: auth(),
            kind: None,
            status: None,
            page: PageRequest::new(1).unwrap(),
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(!msg.to_ascii_lowercase().contains("sqlite"));
    assert!(!msg.to_ascii_lowercase().contains("rusqlite"));
    assert!(!msg.contains('/'));
    let _ = TaskSummary {
        id: TaskId("t".into()),
        kind: TaskKind::Ingest,
        status: TaskStatus::Queued,
        created_at: "0".into(),
        updated_at: "0".into(),
        started_at: None,
        finished_at: None,
        request: serde_json::json!({}),
        result: None,
        error: None,
        queue_position: None,
        blocking_reason: None,
        progress: None,
    };
}
