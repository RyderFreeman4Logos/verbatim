//! Contract tests for snapshot-bound cursors & pagination (API-003 / #354).

use super::*;
use crate::storage_ports::{PageCursor, StorageGeneration};

fn seal_key() -> CursorSealKey {
    CursorSealKey::new(b"test-cursor-seal-key-v1").unwrap()
}

fn sample_claims(mode: PaginationMode) -> CursorClaims {
    CursorClaims::new(CursorClaimsFields {
        mode,
        query_plan_hash: "qp-hash-aaa".into(),
        principal: "user:alice".into(),
        publication_generation: StorageGeneration::new(7),
        profile_ref: "profile:default".into(),
        policy_version: "policy-v3".into(),
        last_sort_key: "score:0.91|id:eu-42".into(),
        page_ordinal: 0,
        expires_at_unix: 1_700_000_100,
        pointer_epoch: None,
        consumer_id: "cursor-consumer-1".into(),
    })
    .unwrap()
}

fn sample_ctx(claims: &CursorClaims, now_unix: u64) -> ContinuationContext {
    ContinuationContext {
        mode: claims.mode,
        query_plan_hash: claims.query_plan_hash.clone(),
        principal: claims.principal.clone(),
        publication_generation: claims.publication_generation(),
        profile_ref: claims.profile_ref.clone(),
        policy_version: claims.policy_version.clone(),
        available_generation: Some(claims.publication_generation()),
        now_unix,
    }
}

#[test]
fn cursor_round_trip_preserves_claims() {
    let key = seal_key();
    let claims = sample_claims(PaginationMode::RankedSearch);
    let cursor = seal_cursor(&claims, &key).unwrap();
    assert!(cursor.0.starts_with("v1."));
    let opened = open_cursor(&cursor, &key).unwrap();
    assert_eq!(opened, claims);
    assert_eq!(opened.schema_version, CURSOR_SCHEMA_VERSION);
    assert_eq!(
        opened.publication_binding.kind,
        crate::index_publication::QueryPublicationBindingKind::Cursor
    );
}

#[test]
fn encode_cursor_alias_matches_seal() {
    let key = seal_key();
    let claims = sample_claims(PaginationMode::ExhaustiveEnumeration);
    let a = seal_cursor(&claims, &key).unwrap();
    let b = encode_cursor(&claims, &key).unwrap();
    // Same claims + key → same payload seal; payload JSON may share structure.
    let oa = open_cursor(&a, &key).unwrap();
    let ob = open_cursor(&b, &key).unwrap();
    assert_eq!(oa, ob);
    assert_eq!(oa.mode, PaginationMode::ExhaustiveEnumeration);
}

#[test]
fn tampered_cursor_is_rejected() {
    let key = seal_key();
    let claims = sample_claims(PaginationMode::RankedSearch);
    let cursor = seal_cursor(&claims, &key).unwrap();
    let mut parts: Vec<String> = cursor.0.split('.').map(str::to_string).collect();
    assert_eq!(parts.len(), 3);
    // Flip a character in the payload segment.
    let mut body = parts[1].clone();
    let flip = if body.ends_with('A') { 'B' } else { 'A' };
    body.pop();
    body.push(flip);
    parts[1] = body;
    let tampered = PageCursor::new(parts.join(".")).unwrap();
    let err = open_cursor(&tampered, &key).unwrap_err();
    assert_eq!(err.class_name(), "invalid");
    assert!(err.to_string().contains("seal"), "{err}");
}

#[test]
fn wrong_seal_key_is_rejected() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let cursor = seal_cursor(&claims, &seal_key()).unwrap();
    let other = CursorSealKey::new(b"other-key").unwrap();
    let err = open_cursor(&cursor, &other).unwrap_err();
    assert_eq!(err.class_name(), "invalid");
}

#[test]
fn principal_mismatch_fails_closed() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let mut ctx = sample_ctx(&claims, 1_700_000_000);
    ctx.principal = "user:bob".into();
    let err = validate_cursor_continuation(&claims, &ctx).unwrap_err();
    assert_eq!(err.class_name(), "unauthorized");
    let storage = err.to_storage_error();
    assert_eq!(storage.class_name(), "unauthorized");
}

#[test]
fn generation_mismatch_and_gone_fail_closed() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let mut ctx = sample_ctx(&claims, 1_700_000_000);
    ctx.publication_generation = StorageGeneration::new(99);
    let err = validate_cursor_continuation(&claims, &ctx).unwrap_err();
    assert_eq!(err.class_name(), "generation_gone");
    assert_eq!(err.to_storage_error().class_name(), "stale_generation");

    let mut ctx2 = sample_ctx(&claims, 1_700_000_000);
    ctx2.available_generation = Some(StorageGeneration::new(1));
    let err2 = validate_cursor_continuation(&claims, &ctx2).unwrap_err();
    assert_eq!(err2.class_name(), "generation_gone");
}

#[test]
fn expired_cursor_fails_closed() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let ctx = sample_ctx(&claims, claims.expires_at_unix + 1);
    let err = validate_cursor_continuation(&claims, &ctx).unwrap_err();
    assert_eq!(err.class_name(), "expired");
}

#[test]
fn profile_and_policy_and_query_mismatches_fail_closed() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let mut ctx = sample_ctx(&claims, 1_700_000_000);
    ctx.profile_ref = "profile:other".into();
    assert_eq!(
        validate_cursor_continuation(&claims, &ctx)
            .unwrap_err()
            .class_name(),
        "profile_changed"
    );

    let mut ctx = sample_ctx(&claims, 1_700_000_000);
    ctx.policy_version = "policy-v9".into();
    assert_eq!(
        validate_cursor_continuation(&claims, &ctx)
            .unwrap_err()
            .class_name(),
        "policy_changed"
    );

    let mut ctx = sample_ctx(&claims, 1_700_000_000);
    ctx.query_plan_hash = "qp-other".into();
    assert_eq!(
        validate_cursor_continuation(&claims, &ctx)
            .unwrap_err()
            .class_name(),
        "query_mismatch"
    );
}

#[test]
fn ranked_and_exhaustive_modes_are_not_interchangeable() {
    let ranked = sample_claims(PaginationMode::RankedSearch);
    let mut ctx = sample_ctx(&ranked, 1_700_000_000);
    ctx.mode = PaginationMode::ExhaustiveEnumeration;
    let err = validate_cursor_continuation(&ranked, &ctx).unwrap_err();
    assert_eq!(err.class_name(), "mode_mismatch");

    let ranked_req = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::RankedSearch,
        limit: 10,
        query_plan_hash: "qp".into(),
        principal: "p".into(),
        publication_generation: StorageGeneration::new(1),
        profile_ref: "profile".into(),
        policy_version: "pol".into(),
        cursor: None,
        pointer_epoch: None,
    })
    .unwrap();
    let exhaustive_req = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::ExhaustiveEnumeration,
        limit: 10,
        query_plan_hash: "qp".into(),
        principal: "p".into(),
        publication_generation: StorageGeneration::new(1),
        profile_ref: "profile".into(),
        policy_version: "pol".into(),
        cursor: None,
        pointer_epoch: None,
    })
    .unwrap();
    assert_ne!(ranked_req.mode, exhaustive_req.mode);

    let page = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        StorageGeneration::new(1),
        vec!["hit-a"],
        None,
        true,
        None,
    );
    assert!(page.exhausted);
    assert_eq!(page.mode, PaginationMode::RankedSearch);
    assert!(page.total_hint.is_none());
    assert!(
        SnapshotPageResponse::<&str>::empty(
            PaginationMode::ExhaustiveEnumeration,
            StorageGeneration::new(2)
        )
        .exhausted
    );
}

#[test]
fn exhausted_last_page_does_not_invent_total_hint_from_page_len() {
    // Multi-page walk: conceptual total 110, last page holds only 10 items.
    let last_page_items = vec![
        "r101", "r102", "r103", "r104", "r105", "r106", "r107", "r108", "r109", "r110",
    ];
    assert_eq!(last_page_items.len(), 10);

    let last = SnapshotPageResponse::page(
        PaginationMode::ExhaustiveEnumeration,
        StorageGeneration::new(5),
        last_page_items.clone(),
        None,
        true,
        None,
    );
    assert!(last.exhausted);
    assert!(last.next_cursor.is_none());
    assert_eq!(last.items.len(), 10);
    assert!(
        last.total_hint.is_none(),
        "generic helper must not invent total_hint from last-page length"
    );

    let with_known_total = SnapshotPageResponse::page(
        PaginationMode::ExhaustiveEnumeration,
        StorageGeneration::new(5),
        last_page_items,
        None,
        true,
        Some(110),
    );
    assert_eq!(with_known_total.total_hint, Some(110));

    let mid = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        StorageGeneration::new(5),
        vec!["hit"; 50],
        Some(PageCursor::new("opaque-next").unwrap()),
        false,
        None,
    );
    assert!(!mid.exhausted);
    assert!(mid.total_hint.is_none());

    // Empty snapshot is the one case where a known total of 0 is correct.
    let empty = SnapshotPageResponse::<&str>::empty(
        PaginationMode::RankedSearch,
        StorageGeneration::new(1),
    );
    assert_eq!(empty.total_hint, Some(0));
}

#[test]
fn happy_continuation_validates() {
    let claims = sample_claims(PaginationMode::RankedSearch);
    let key = seal_key();
    let cursor = seal_cursor(&claims, &key).unwrap();
    let opened = open_cursor(&cursor, &key).unwrap();
    validate_cursor_continuation(&opened, &sample_ctx(&opened, 1_700_000_000)).unwrap();
}

#[test]
fn snapshot_page_request_rejects_zero_limit_and_builds_binding() {
    let err = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::RankedSearch,
        limit: 0,
        query_plan_hash: "qp".into(),
        principal: "p".into(),
        publication_generation: StorageGeneration::new(3),
        profile_ref: "profile".into(),
        policy_version: "pol".into(),
        cursor: None,
        pointer_epoch: None,
    })
    .unwrap_err();
    assert_eq!(err.class_name(), "invalid_request");

    let req = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::ExhaustiveEnumeration,
        limit: DEFAULT_SNAPSHOT_PAGE_LIMIT,
        query_plan_hash: "qp".into(),
        principal: "p".into(),
        publication_generation: StorageGeneration::new(3),
        profile_ref: "profile".into(),
        policy_version: "pol".into(),
        cursor: Some(PageCursor::new("opaque").unwrap()),
        pointer_epoch: None,
    })
    .unwrap();
    assert!(req.is_continuation());
    let binding = req.publication_binding("c-9").unwrap();
    assert_eq!(binding.publication_generation, StorageGeneration::new(3));
    assert_eq!(
        binding.kind,
        crate::index_publication::QueryPublicationBindingKind::Cursor
    );
}

#[test]
fn mutation_idempotent_retry_returns_same_result() {
    let mut reg = InMemoryIdempotencyRegistry::new();
    let key = MutationIdempotencyKey::new("idem-1").unwrap();
    let op = MutationOperationFingerprint::new("upload:src-1:sha-abc").unwrap();
    let result = MutationResultToken::new("op-result-42").unwrap();

    assert_eq!(
        reg.claim("user:alice", &key, &op).unwrap(),
        IdempotencyClaim::Fresh
    );
    assert_eq!(
        reg.claim("user:alice", &key, &op).unwrap(),
        IdempotencyClaim::InProgress
    );
    reg.complete("user:alice", &key, &op, result.clone())
        .unwrap();
    match reg.claim("user:alice", &key, &op).unwrap() {
        IdempotencyClaim::Replay { result: replayed } => {
            assert_eq!(replayed, result);
        }
        other => panic!("expected replay, got {other:?}"),
    }
    // Completing again with the same token is a no-op.
    reg.complete("user:alice", &key, &op, result).unwrap();
}

#[test]
fn mutation_idempotency_rejects_key_reuse_with_different_fingerprint() {
    let mut reg = InMemoryIdempotencyRegistry::new();
    let key = MutationIdempotencyKey::new("idem-2").unwrap();
    let op_a = MutationOperationFingerprint::new("delete:a").unwrap();
    let op_b = MutationOperationFingerprint::new("delete:b").unwrap();
    reg.claim("user:alice", &key, &op_a).unwrap();
    let err = reg.claim("user:alice", &key, &op_b).unwrap_err();
    match err {
        IdempotencyError::Conflict { .. } => {}
        other => panic!("expected conflict, got {other:?}"),
    }
    assert_eq!(err.to_storage_error().class_name(), "conflict");
}

#[test]
fn mutation_complete_without_claim_and_result_token_conflict() {
    let mut reg = InMemoryIdempotencyRegistry::new();
    let key = MutationIdempotencyKey::new("idem-3").unwrap();
    let op = MutationOperationFingerprint::new("upload:x").unwrap();
    let result_a = MutationResultToken::new("token-a").unwrap();
    let result_b = MutationResultToken::new("token-b").unwrap();

    let err = reg
        .complete("user:alice", &key, &op, result_a.clone())
        .unwrap_err();
    match err {
        IdempotencyError::Invalid { detail } => {
            assert!(detail.contains("claim first"), "{detail}");
        }
        other => panic!("expected invalid (complete without claim), got {other:?}"),
    }

    assert_eq!(
        reg.claim("user:alice", &key, &op).unwrap(),
        IdempotencyClaim::Fresh
    );
    reg.complete("user:alice", &key, &op, result_a).unwrap();
    let err = reg.complete("user:alice", &key, &op, result_b).unwrap_err();
    match err {
        IdempotencyError::Conflict { detail, .. } => {
            assert!(detail.contains("different result token"), "{detail}");
        }
        other => panic!("expected conflict on result token mismatch, got {other:?}"),
    }
}

#[test]
fn generation_gone_without_available_does_not_fabricate_initial() {
    let err = CursorError::generation_gone(
        StorageGeneration::new(7),
        None,
        "bound generation no longer retained",
    );
    let storage = err.to_storage_error();
    assert_eq!(storage.class_name(), "unavailable");
    assert!(
        !matches!(
            storage,
            crate::storage_ports::StorageError::StaleGeneration {
                actual: StorageGeneration::INITIAL,
                ..
            }
        ),
        "must not invent generation 0 as actual when available is None: {storage:?}"
    );
}

#[test]
fn empty_seal_key_and_empty_idempotency_key_rejected() {
    assert!(CursorSealKey::new(b"").is_err());
    assert!(MutationIdempotencyKey::new("").is_err());
    assert!(MutationIdempotencyKey::new("   ").is_err());
    assert!(MutationOperationFingerprint::new("").is_err());
    assert!(MutationResultToken::new("").is_err());
}

#[test]
fn decode_cursor_claims_rejects_unknown_schema() {
    let mut claims = sample_claims(PaginationMode::RankedSearch);
    claims.schema_version = 99;
    let bytes = serde_json::to_vec(&claims).unwrap();
    let err = decode_cursor_claims(&bytes).unwrap_err();
    assert_eq!(err.class_name(), "invalid");
}
