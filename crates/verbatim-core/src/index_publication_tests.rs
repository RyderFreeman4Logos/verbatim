//! Unit/contract tests for index publication walking skeleton (DIST-006 / #352).

use super::*;
use crate::storage_ports::{StorageError, StorageGeneration};
use crate::types::EmbeddingProfileId;

fn gen(n: u64) -> StorageGeneration {
    StorageGeneration::new(n)
}

fn profile(id: &str) -> EmbeddingProfileId {
    EmbeddingProfileId::new(id).expect("profile")
}

fn source(id: &str, digest: &str) -> SourceSnapshotRef {
    SourceSnapshotRef::new(id, digest).expect("source")
}

fn digest(
    kind: ComponentKind,
    generation: StorageGeneration,
    content_digest: &str,
) -> ComponentDigest {
    ComponentDigest::new(kind, generation, content_digest).expect("digest")
}

/// Ready lexical+vector publication generation `n`.
fn ready_manifest(generation: u64) -> IndexPublicationManifest {
    let g = gen(generation);
    IndexPublicationManifest::new(IndexPublicationManifestFields {
        generation: g,
        source_snapshots: vec![source(
            "src-a",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )],
        evidence_generation: g,
        catalog_generation: g,
        lexical_generation: Some(g),
        vector_generation: Some(g),
        graph_generation: None,
        profile_id: Some(profile("default")),
        embedding_model_id: Some("text-embedding-3-small".into()),
        capabilities: DeclaredCapabilities {
            evidence: true,
            catalog: true,
            lexical: true,
            vector: true,
            graph: false,
        },
        component_digests: vec![
            digest(
                ComponentKind::Evidence,
                g,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            digest(
                ComponentKind::Catalog,
                g,
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            ),
            digest(
                ComponentKind::Lexical,
                g,
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            ),
            digest(
                ComponentKind::Vector,
                g,
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            ),
        ],
        acl_policy_version: "acl-v1".into(),
        lifecycle_policy_version: "life-v1".into(),
        status: BuildStatus::Ready,
        built_at: format!("2026-07-26T00:00:{generation:02}Z"),
        build_metadata: Some("builder-1".into()),
    })
    .expect("manifest")
}

fn building_manifest(generation: u64) -> IndexPublicationManifest {
    let mut m = ready_manifest(generation);
    m.status = BuildStatus::Building;
    m
}

fn initial_pointer() -> ActiveGenerationPointer {
    ActiveGenerationPointer::new(gen(0), PointerEpoch::INITIAL, "2026-07-26T00:00:00Z")
        .expect("pointer")
}

// ---------------------------------------------------------------------------
// Schema fail-closed
// ---------------------------------------------------------------------------

#[test]
fn unknown_schema_version_fails_closed_on_decode() {
    let mut m = ready_manifest(1);
    m.schema_version = 99;
    let bytes = serde_json::to_vec(&m).expect("encode");
    let err = decode_index_publication_manifest_json(&bytes).expect_err("must refuse");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
    assert!(err.to_string().contains("unsupported"));
}

#[test]
fn unknown_pointer_schema_fails_closed() {
    let mut p = initial_pointer();
    p.schema_version = 7;
    let bytes = serde_json::to_vec(&p).expect("encode");
    let err = decode_active_generation_pointer_json(&bytes).expect_err("must refuse");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn completeness_requires_declared_components() {
    let mut m = ready_manifest(2);
    m.component_digests
        .retain(|d| d.kind != ComponentKind::Vector);
    let report = validate_completeness(&m);
    assert!(!report.is_ok());
    assert!(report.issues.iter().any(|i| i.code == "missing_component"));
}

#[test]
fn hash_mismatch_rejects_empty_or_whitespace_digest() {
    let err = ComponentDigest::new(ComponentKind::Lexical, gen(1), "  ").expect_err("empty");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));

    let mut m = ready_manifest(3);
    // Force a bad digest past constructor for validator path.
    m.component_digests[0].digest = String::new();
    let report = validate_hash_integrity(&m);
    assert!(!report.is_ok());
    assert!(report
        .issues
        .iter()
        .any(|i| i.code == "component_digest_invalid"));
}

#[test]
fn referential_integrity_detects_generation_mismatch() {
    let mut m = ready_manifest(4);
    // Lexical component claims wrong generation.
    if let Some(d) = m
        .component_digests
        .iter_mut()
        .find(|d| d.kind == ComponentKind::Lexical)
    {
        d.generation = gen(99);
    }
    let report = validate_referential_integrity(&m);
    assert!(!report.is_ok());
    assert!(report
        .issues
        .iter()
        .any(|i| i.code == "generation_mismatch"));
}

#[test]
fn profile_compatibility_requires_profile_for_vector() {
    let mut m = ready_manifest(5);
    m.profile_id = None;
    let report = validate_profile_compatibility(&m, None);
    assert!(!report.is_ok());
    assert!(report.issues.iter().any(|i| i.code == "missing_profile"));

    m.profile_id = Some(profile("other"));
    let report = validate_profile_compatibility(&m, Some(&profile("default")));
    assert!(!report.is_ok());
    assert!(report.issues.iter().any(|i| i.code == "profile_mismatch"));
}

// ---------------------------------------------------------------------------
// Promotion happy path + reject incomplete + concurrent conflict + rollback
// ---------------------------------------------------------------------------

#[test]
fn happy_promote_stages_validates_and_cas_promotes() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    let m = ready_manifest(1);
    coord.stage(m.clone()).expect("stage");
    coord
        .validate(gen(1), Some(&profile("default")))
        .expect("validate");

    let outcome = coord
        .promote(
            gen(1),
            gen(0),
            PointerEpoch::INITIAL,
            "2026-07-26T00:01:00Z",
            Some(&profile("default")),
        )
        .expect("promote");

    match outcome {
        PromotionOutcome::Promoted {
            pointer,
            generation,
        } => {
            assert_eq!(generation, gen(1));
            assert_eq!(pointer.active_generation, gen(1));
            assert_eq!(pointer.epoch, PointerEpoch::new(1));
            assert_eq!(pointer.previous_generation, Some(gen(0)));
        }
        other => panic!("expected Promoted, got {other:?}"),
    }
    assert_eq!(coord.active_pointer().active_generation, gen(1));
    assert_eq!(
        coord.get_manifest(gen(1)).expect("m").status,
        BuildStatus::Active
    );
}

#[test]
fn incomplete_building_generation_cannot_promote() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    coord.stage(building_manifest(1)).expect("stage");

    let outcome = coord
        .promote(
            gen(1),
            gen(0),
            PointerEpoch::INITIAL,
            "2026-07-26T00:01:00Z",
            None,
        )
        .expect("promote call");

    match outcome {
        PromotionOutcome::Rejected { reason } => {
            assert!(
                reason.contains("status_not_promotable") || reason.contains("incomplete"),
                "reason={reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(coord.active_pointer().active_generation, gen(0));
}

#[test]
fn hash_mismatch_blocks_promotion() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    let mut m = ready_manifest(2);
    m.component_digests[0].digest = String::new();
    // Staging requires structural validation — empty digest fails stage.
    let err = coord.stage(m).expect_err("stage must fail");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
}

#[test]
fn concurrent_promote_conflict_is_typed_stale() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    coord.stage(ready_manifest(1)).expect("stage");
    // First promoter wins.
    let first = coord
        .promote(
            gen(1),
            gen(0),
            PointerEpoch::INITIAL,
            "t1",
            Some(&profile("default")),
        )
        .expect("first");
    assert!(matches!(first, PromotionOutcome::Promoted { .. }));

    // Second promoter still holds stale (gen0, epoch0).
    coord.stage(ready_manifest(2)).expect("stage2");
    let second = coord
        .promote(
            gen(2),
            gen(0),
            PointerEpoch::INITIAL,
            "t2",
            Some(&profile("default")),
        )
        .expect("second");

    match second {
        PromotionOutcome::Conflict(c) => {
            assert_eq!(c.expected_generation, gen(0));
            assert_eq!(c.actual_generation, gen(1));
            assert_eq!(c.expected_epoch, PointerEpoch::INITIAL);
            assert_eq!(c.actual_epoch, PointerEpoch::new(1));
            let err = c.to_storage_error();
            assert!(matches!(err, StorageError::StaleGeneration { .. }));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn rollback_restores_previous_generation() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    coord.stage(ready_manifest(1)).expect("stage1");
    coord
        .promote(
            gen(1),
            gen(0),
            PointerEpoch::INITIAL,
            "t1",
            Some(&profile("default")),
        )
        .expect("p1");

    let epoch1 = coord.active_pointer().epoch;
    coord.stage(ready_manifest(2)).expect("stage2");
    let p2 = coord
        .promote(gen(2), gen(1), epoch1, "t2", Some(&profile("default")))
        .expect("p2");
    assert!(matches!(p2, PromotionOutcome::Promoted { .. }));

    let epoch2 = coord.active_pointer().epoch;
    let rb = coord.rollback(gen(2), epoch2, "t3").expect("rollback");
    match rb {
        PromotionOutcome::RolledBack {
            restored_generation,
            pointer,
        } => {
            assert_eq!(restored_generation, gen(1));
            assert_eq!(pointer.active_generation, gen(1));
            assert_eq!(coord.active_pointer().active_generation, gen(1));
            assert_eq!(
                coord.get_manifest(gen(2)).expect("m2").status,
                BuildStatus::RolledBack
            );
        }
        other => panic!("expected RolledBack, got {other:?}"),
    }
}

#[test]
fn restage_of_active_generation_is_rejected() {
    let mut coord = InMemoryPublicationCoordinator::new(initial_pointer()).expect("coord");
    coord.stage(ready_manifest(1)).expect("stage");
    let outcome = coord
        .promote(
            gen(1),
            gen(0),
            PointerEpoch::INITIAL,
            "2026-07-26T00:01:00Z",
            Some(&profile("default")),
        )
        .expect("promote");
    assert!(matches!(outcome, PromotionOutcome::Promoted { .. }));
    assert_eq!(coord.active_pointer().active_generation, gen(1));
    assert_eq!(
        coord.get_manifest(gen(1)).expect("m").status,
        BuildStatus::Active
    );

    let err = coord
        .stage(ready_manifest(1))
        .expect_err("restaging active generation must fail closed");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
    assert!(
        err.to_string().contains("active generation")
            || err.to_string().contains("already marked Active"),
        "err={err}"
    );

    // Pointer and registry must be unchanged on reject.
    assert_eq!(coord.active_pointer().active_generation, gen(1));
    assert_eq!(
        coord.get_manifest(gen(1)).expect("m").status,
        BuildStatus::Active
    );
}

// ---------------------------------------------------------------------------
// Reconciliation findings
// ---------------------------------------------------------------------------

#[test]
fn reconciliation_findings_are_typed() {
    let divergent =
        ReconciliationFinding::divergent_component(ComponentKind::Lexical, gen(1), gen(2))
            .expect("divergent");
    assert!(matches!(
        divergent.finding,
        ReconciliationFindingKind::DivergentComponent { .. }
    ));
    assert_eq!(divergent.severity, ReconciliationSeverity::Critical);

    let missing = ReconciliationFinding::missing_chunk("chunk-1", gen(1)).expect("missing");
    assert!(matches!(
        missing.finding,
        ReconciliationFindingKind::MissingChunk { .. }
    ));

    let hash =
        ReconciliationFinding::hash_mismatch(ComponentKind::Vector, "aa", "bb").expect("hash");
    assert!(matches!(
        hash.finding,
        ReconciliationFindingKind::HashMismatch { .. }
    ));

    let acl = ReconciliationFinding::stale_acl_policy("acl-v2", "acl-v1").expect("acl");
    assert!(matches!(
        acl.finding,
        ReconciliationFindingKind::StaleAclPolicy { .. }
    ));

    let orphan = ReconciliationFinding::orphan_generation(gen(9), "gens/9").expect("orphan");
    assert!(matches!(
        orphan.finding,
        ReconciliationFindingKind::OrphanGeneration { .. }
    ));

    let mut report = ReconciliationReport::new();
    report.push(divergent);
    report.push(missing);
    report.push(hash);
    report.push(acl);
    report.push(orphan);
    assert!(!report.is_clean());
    assert!(report.has_critical());
    assert_eq!(report.findings.len(), 5);
}

// ---------------------------------------------------------------------------
// Query binding
// ---------------------------------------------------------------------------

#[test]
fn query_binding_pins_single_generation() {
    let b = QueryPublicationBinding::new(QueryPublicationBindingKind::Cursor, gen(3), "cursor-9")
        .expect("bind")
        .with_pointer_epoch(PointerEpoch::new(4));
    assert_eq!(b.publication_generation, gen(3));
    assert_eq!(b.pointer_epoch, Some(PointerEpoch::new(4)));
    b.validate().expect("ok");

    let bytes = serde_json::to_vec(&b).expect("encode");
    let round = decode_query_publication_binding_json(&bytes).expect("decode");
    assert_eq!(round.consumer_id, "cursor-9");

    let mut bad = b.clone();
    bad.schema_version = 99;
    let err_bytes = serde_json::to_vec(&bad).expect("encode");
    assert!(decode_query_publication_binding_json(&err_bytes).is_err());
}

#[test]
fn failed_status_cannot_promote_via_validator() {
    let mut m = ready_manifest(8);
    m.status = BuildStatus::Failed;
    let report = validate_for_promotion(&m, None);
    assert!(!report.is_ok());
    assert!(report
        .issues
        .iter()
        .any(|i| i.code == "status_not_promotable" || i.code == "incomplete_or_failed"));
    assert!(!m.status.can_promote());
}
