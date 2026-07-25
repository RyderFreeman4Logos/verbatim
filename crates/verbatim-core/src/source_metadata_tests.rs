//! Contract tests for typed source/evidence metadata (META-001 / issue #336).

use super::*;

fn field(
    name: MetadataFieldName,
    value: MetadataValue,
    origin: MetadataOrigin,
    confidence: MetadataConfidence,
    extractor: &str,
    observed_at: u64,
) -> SourceMetadataField {
    SourceMetadataField::new(SourceMetadataFieldParams {
        name,
        value,
        origin,
        confidence,
        extractor_id: extractor.into(),
        observed_at_unix: observed_at,
        scope: MetadataScope::Source,
        reason: "test".into(),
    })
    .unwrap()
}

#[test]
fn user_override_beats_front_matter_and_filename_for_title() {
    let mut meta = SourceMetadata::new();
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::Title,
            MetadataValue::Text("from-fm".into()),
            MetadataOrigin::FrontMatter,
            MetadataConfidence::High,
            "fm",
            10,
        ))
        .unwrap());
    // Filename never becomes winner.
    assert!(!meta
        .apply_candidate(
            SourceMetadataField::filename_hint(
                MetadataFieldName::Title,
                MetadataValue::Text("model-generated-name".into()),
                "fs",
                20,
                MetadataScope::Source,
            )
            .unwrap()
        )
        .unwrap());
    assert_eq!(
        meta.fields.get("title").unwrap().value,
        MetadataValue::Text("from-fm".into())
    );
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::Title,
            MetadataValue::Text("user-title".into()),
            MetadataOrigin::User,
            MetadataConfidence::High,
            "user",
            30,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("title").unwrap().value,
        MetadataValue::Text("user-title".into())
    );
    assert!(meta
        .superseded
        .iter()
        .any(|f| matches!(f.origin, MetadataOrigin::Filesystem)));
}

#[test]
fn conflicting_url_fs_time_and_user_follow_precedence() {
    let mut meta = SourceMetadata::new();
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::OriginUrl,
            MetadataValue::Url("https://example.com/a".into()),
            MetadataOrigin::Parser,
            MetadataConfidence::Medium,
            "parser",
            1,
        ))
        .unwrap());
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::OriginUrl,
            MetadataValue::Url("https://example.com/b".into()),
            MetadataOrigin::SourceNative,
            MetadataConfidence::High,
            "native",
            2,
        ))
        .unwrap());
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::OriginUrl,
            MetadataValue::Url("https://example.com/c".into()),
            MetadataOrigin::ModelDerived,
            MetadataConfidence::Low,
            "model",
            3,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("origin_url").unwrap().value,
        MetadataValue::Url("https://example.com/b".into())
    );

    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::ModifiedAt,
            MetadataValue::DateTime("2024-01-01T00:00:00Z".into()),
            MetadataOrigin::Filesystem,
            MetadataConfidence::Medium,
            "mtime",
            1,
        ))
        .unwrap());
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::ModifiedAt,
            MetadataValue::DateTime("2024-06-01T12:00:00+08:00".into()),
            MetadataOrigin::FrontMatter,
            MetadataConfidence::High,
            "fm",
            2,
        ))
        .unwrap());
    // Timezone-preserving string: +08:00 retained, not coerced to Z.
    assert_eq!(
        meta.fields.get("modified_at").unwrap().value,
        MetadataValue::DateTime("2024-06-01T12:00:00+08:00".into())
    );
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::ModifiedAt,
            MetadataValue::DateTime("2025-01-01T00:00:00Z".into()),
            MetadataOrigin::User,
            MetadataConfidence::High,
            "user",
            3,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("modified_at").unwrap().value,
        MetadataValue::DateTime("2025-01-01T00:00:00Z".into())
    );
}

#[test]
fn missing_datetime_rejected_by_strict_query() {
    let mut meta = SourceMetadata::new();
    meta.apply_candidate(field(
        MetadataFieldName::PublishedAt,
        MetadataValue::DateTime("".into()),
        MetadataOrigin::FrontMatter,
        MetadataConfidence::Low,
        "fm",
        1,
    ))
    .unwrap();
    let err = meta
        .require_field(&MetadataFieldName::PublishedAt)
        .expect_err("empty datetime must fail strict query");
    assert!(err.to_string().contains("missing datetime"));
}

#[test]
fn model_derived_cannot_set_or_weaken_lifecycle_acl() {
    let mut meta = SourceMetadata::new();
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::Lifecycle,
            MetadataValue::LifecycleState("public".into()),
            MetadataOrigin::ModelDerived,
            MetadataConfidence::Medium,
            "model",
            1,
        ))
        .unwrap());
    assert!(meta.fields.get("lifecycle").is_none());
    assert!(!meta.superseded.is_empty());

    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::Lifecycle,
            MetadataValue::LifecycleState("legal_hold".into()),
            MetadataOrigin::User,
            MetadataConfidence::High,
            "user",
            2,
        ))
        .unwrap());
    // Lower/peer trust must not weaken a more restrictive protected state.
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::Lifecycle,
            MetadataValue::LifecycleState("public".into()),
            MetadataOrigin::FrontMatter,
            MetadataConfidence::High,
            "fm",
            3,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("lifecycle").unwrap().value,
        MetadataValue::LifecycleState("legal_hold".into())
    );

    // ACL: model cannot install.
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::Acl,
            MetadataValue::Text("public".into()),
            MetadataOrigin::ModelDerived,
            MetadataConfidence::High,
            "model",
            4,
        ))
        .unwrap());
    assert!(meta.fields.get("acl").is_none());
}

#[test]
fn filename_hint_never_authoritative_for_benchmark_grouping() {
    let mut meta = SourceMetadata::new();
    let hint = markdown_thread_filename_hint(
        "corpus/recent-tech/mdl-gen-title-abc123.md",
        "md-thread",
        100,
        MetadataScope::Source,
    )
    .unwrap();
    assert_eq!(hint.confidence, MetadataConfidence::HintOnly);
    assert_eq!(hint.origin, MetadataOrigin::Filesystem);
    assert!(!meta.apply_candidate(hint).unwrap());
    // No winning title from filename alone — benchmark gold independent of path.
    assert!(meta.fields.get("title").is_none());
    assert!(meta
        .require_field(&MetadataFieldName::Title)
        .unwrap_err()
        .to_string()
        .contains("missing required"));

    // Real title can still be installed from front matter.
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::Title,
            MetadataValue::Text("Human reviewed title".into()),
            MetadataOrigin::FrontMatter,
            MetadataConfidence::High,
            "fm",
            101,
        ))
        .unwrap());
    assert_eq!(
        meta.require_field(&MetadataFieldName::Title).unwrap().value,
        MetadataValue::Text("Human reviewed title".into())
    );
}

#[test]
fn duplicate_thread_id_keeps_higher_trust_origin() {
    let mut meta = SourceMetadata::new();
    assert!(meta
        .apply_candidate(field(
            MetadataFieldName::ThreadId,
            MetadataValue::Text("thread-1".into()),
            MetadataOrigin::SourceNative,
            MetadataConfidence::High,
            "native",
            1,
        ))
        .unwrap());
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::ThreadId,
            MetadataValue::Text("thread-2".into()),
            MetadataOrigin::ModelDerived,
            MetadataConfidence::High,
            "model",
            2,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("thread_id").unwrap().value,
        MetadataValue::Text("thread-1".into())
    );
}

#[test]
fn multilingual_title_and_malformed_version_types_are_carried() {
    let mut meta = SourceMetadata::new();
    meta.apply_candidate(field(
        MetadataFieldName::Title,
        MetadataValue::Text("日本語タイトル / English subtitle".into()),
        MetadataOrigin::FrontMatter,
        MetadataConfidence::High,
        "fm",
        1,
    ))
    .unwrap();
    // Product version is free-form text at the contract layer; adapters may
    // reject malformed versions at a later validation stage.
    meta.apply_candidate(field(
        MetadataFieldName::ProductVersion,
        MetadataValue::ProductVersion("not-a-semver!!!".into()),
        MetadataOrigin::FrontMatter,
        MetadataConfidence::Low,
        "fm",
        1,
    ))
    .unwrap();
    assert!(matches!(
        meta.fields.get("title").unwrap().value,
        MetadataValue::Text(ref s) if s.contains("日本語")
    ));
    assert_eq!(
        meta.fields.get("product_version").unwrap().value,
        MetadataValue::ProductVersion("not-a-semver!!!".into())
    );
}

#[test]
fn serialization_roundtrip_and_unknown_schema_fail_closed() {
    let mut meta = SourceMetadata::new();
    meta.apply_candidate(field(
        MetadataFieldName::Language,
        MetadataValue::LanguageTag("zh-CN".into()),
        MetadataOrigin::Parser,
        MetadataConfidence::Medium,
        "langdetect",
        9,
    ))
    .unwrap();
    let bytes = serde_json::to_vec(&meta).unwrap();
    let back = decode_source_metadata_json(&bytes).unwrap();
    assert_eq!(back, meta);

    let mut bad = meta;
    bad.schema_version = 99;
    let bad_bytes = serde_json::to_vec(&bad).unwrap();
    let err = decode_source_metadata_json(&bad_bytes).expect_err("must fail closed");
    assert!(err
        .to_string()
        .contains("unsupported source metadata schema version 99"));
}

#[test]
fn approved_fields_exclude_hint_only() {
    let mut meta = SourceMetadata::new();
    // Force-install a hint by direct map insert is not allowed via apply;
    // approved projection filters HintOnly if present.
    let mut hint = field(
        MetadataFieldName::Mime,
        MetadataValue::MimeType("text/markdown".into()),
        MetadataOrigin::DeterministicRule,
        MetadataConfidence::HintOnly,
        "ext",
        1,
    );
    hint.confidence = MetadataConfidence::HintOnly;
    meta.fields.insert(hint.wire_key(), hint);
    meta.apply_candidate(field(
        MetadataFieldName::Language,
        MetadataValue::LanguageTag("en".into()),
        MetadataOrigin::SourceNative,
        MetadataConfidence::High,
        "native",
        1,
    ))
    .unwrap();
    let approved: Vec<_> = meta.approved_fields().map(|(k, _)| k.as_str()).collect();
    assert!(approved.contains(&"language"));
    assert!(!approved.contains(&"mime"));
}

#[test]
fn lower_trust_filesystem_cannot_override_source_native_dates() {
    let mut meta = SourceMetadata::new();
    meta.apply_candidate(field(
        MetadataFieldName::PublishedAt,
        MetadataValue::DateTime("2020-01-01T00:00:00Z".into()),
        MetadataOrigin::SourceNative,
        MetadataConfidence::High,
        "native",
        1,
    ))
    .unwrap();
    assert!(!meta
        .apply_candidate(field(
            MetadataFieldName::PublishedAt,
            MetadataValue::DateTime("2024-12-31T23:59:59Z".into()),
            MetadataOrigin::Filesystem,
            MetadataConfidence::High,
            "mtime",
            99,
        ))
        .unwrap());
    assert_eq!(
        meta.fields.get("published_at").unwrap().value,
        MetadataValue::DateTime("2020-01-01T00:00:00Z".into())
    );
}
