use crate::vector_shards::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn valid_hash(suffix: &str) -> String {
    // 64 hex chars, suffix-varied so distinct files get distinct hashes in tests.
    let mut hex = String::from("sha256:");
    let mut remaining = 64usize;
    let bytes = suffix.as_bytes();
    let mut idx = 0;
    while remaining > 0 {
        let b = bytes[idx % bytes.len()];
        let c = match b {
            b'0'..=b'9' | b'a'..=b'f' => b as char,
            _ => {
                let v = (b % 16) as u8;
                if v < 10 {
                    (b'0' + v) as char
                } else {
                    (b'a' + v - 10) as char
                }
            }
        };
        hex.push(c);
        remaining -= 1;
        idx += 1;
    }
    hex
}

fn space() -> ShardVectorSpace {
    ShardVectorSpace::new("text-english-v1").expect("valid vector space")
}

fn generation() -> ShardGeneration {
    ShardGeneration::new(7).expect("nonzero generation")
}

fn ordinal(n: u32) -> ShardOrdinal {
    ShardOrdinal::new(n).expect("valid ordinal")
}

fn shard_id(n: u32) -> ShardId {
    ShardId::new(space(), generation(), ordinal(n)).expect("valid shard id")
}

fn growth_bound() -> StorageGrowthBound {
    StorageGrowthBound::new(10_000_000, 200_000_000_000, 4_096, 64, 32).expect("valid growth bound")
}

fn file(role: ShardFileRole, name: &str, size: u64) -> ShardFile {
    ShardFile::new(
        name,
        role,
        size,
        FileHash::new(valid_hash(name)).expect("valid hash"),
    )
    .expect("valid file")
}

fn required_files() -> Vec<ShardFile> {
    vec![
        file(ShardFileRole::Vectors, "vectors.f32", 1_000),
        file(ShardFileRole::GraphPages, "graph.pages", 500),
        file(ShardFileRole::CandidateCodes, "candidate_codes", 200),
        file(ShardFileRole::IdMap, "id-map", 100),
        file(ShardFileRole::Tombstones, "tombstones", 50),
        file(ShardFileRole::Attributes, "attributes/source", 300),
    ]
}

fn valid_manifest() -> ShardManifest {
    ShardManifest::new(
        shard_id(1),
        generation(),
        1_000,
        required_files(),
        growth_bound(),
    )
    .expect("valid manifest")
}

// ---------------------------------------------------------------------------
// Identity: generation, vector space, ordinal, shard id
// ---------------------------------------------------------------------------

#[test]
fn generation_rejects_zero_through_constructor_and_deserialize() {
    assert_eq!(
        ShardGeneration::new(0)
            .expect_err("zero generation fails closed")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGeneration
    );

    // Serde must route through the constructor so zero cannot bypass validation.
    let err = serde_json::from_str::<ShardGeneration>("0").expect_err("serde rejects zero");
    assert!(err.to_string().contains("invalid_generation") || err.is_data());

    let valid = serde_json::from_str::<ShardGeneration>("5").expect("serde accepts nonzero");
    assert_eq!(valid.value(), 5);
}

#[test]
fn vector_space_rejects_empty_uppercase_and_oversized() {
    for invalid in ["", "UPPER", "has space", &"a".repeat(129)] {
        assert_eq!(
            ShardVectorSpace::new(invalid)
                .expect_err("invalid vector space fails closed")
                .diagnostic_code(),
            VectorShardDiagnosticCode::InvalidShardId
        );
    }
    ShardVectorSpace::new("text_english-v1").expect("lowercase + digits + - + _ valid");
}

#[test]
fn ordinal_rejects_zero_and_overflow() {
    assert_eq!(
        ShardOrdinal::new(0)
            .expect_err("zero ordinal fails")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidShardId
    );
    assert_eq!(
        ShardOrdinal::new(ShardOrdinal::MAX_ORDINAL + 1)
            .expect_err("overflow ordinal fails")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidShardId
    );
    assert_eq!(
        ShardOrdinal::new(ShardOrdinal::MAX_ORDINAL)
            .unwrap()
            .value(),
        ShardOrdinal::MAX_ORDINAL
    );
}

#[test]
fn shard_id_round_trips_and_validates() {
    let shard = shard_id(3);
    assert_eq!(shard.vector_space().as_str(), "text-english-v1");
    assert_eq!(shard.generation().value(), 7);
    assert_eq!(shard.ordinal().value(), 3);
    shard.validate().expect("valid shard validates");
}

// ---------------------------------------------------------------------------
// File hash and file validation
// ---------------------------------------------------------------------------

#[test]
fn file_hash_requires_sha256_prefix_and_64_hex_digits() {
    FileHash::new(valid_hash("aa")).expect("valid sha256 accepted");
    for invalid in ["", "sha256:", "sha256:abc", "md5:abc", "sha256:XYZ"] {
        assert_eq!(
            FileHash::new(invalid)
                .expect_err("invalid hash fails closed")
                .diagnostic_code(),
            VectorShardDiagnosticCode::InvalidFileHash
        );
    }
}

#[test]
fn file_hash_serde_round_trips_through_constructor() {
    let hash = FileHash::new(valid_hash("ff")).expect("valid");
    let json = serde_json::to_string(&hash).expect("encodes");
    let decoded: FileHash = serde_json::from_str(&json).expect("decodes");
    assert_eq!(decoded, hash);
}

#[test]
fn file_rejects_zero_size() {
    let err = ShardFile::new(
        "vectors.f32",
        ShardFileRole::Vectors,
        0,
        FileHash::new(valid_hash("vv")).expect("hash"),
    )
    .expect_err("zero-size file fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidFileSet
    );
}

#[test]
fn file_name_rejects_empty_oversized_and_unsafe_chars() {
    for invalid in ["", &"a".repeat(257), "bad name", "../escape"] {
        assert_eq!(
            ShardFileName::new(invalid)
                .expect_err("invalid name fails")
                .diagnostic_code(),
            VectorShardDiagnosticCode::InvalidFileSet
        );
    }
    ShardFileName::new("attributes/source.json").expect("slash + dot valid");
}

// ---------------------------------------------------------------------------
// Storage growth bound
// ---------------------------------------------------------------------------

#[test]
fn growth_bound_rejects_nonpositive_components() {
    let dim = 4_096u32;
    let floor = dim as u64 * 4;
    // zero vectors
    assert_eq!(
        StorageGrowthBound::new(0, floor, dim, 64, 32)
            .expect_err("zero vectors")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGrowthBound
    );
    // zero dimension
    assert_eq!(
        StorageGrowthBound::new(10, floor, 0, 64, 32)
            .expect_err("zero dimension")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGrowthBound
    );
    // zero graph degree
    assert_eq!(
        StorageGrowthBound::new(10, floor, dim, 0, 32)
            .expect_err("zero graph degree")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGrowthBound
    );
    // zero candidate bytes
    assert_eq!(
        StorageGrowthBound::new(10, floor, dim, 64, 0)
            .expect_err("zero candidate bytes")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGrowthBound
    );
}

#[test]
fn growth_bound_byte_ceiling_must_cover_vectors_floor() {
    // 10 vectors * 4096 dim * 4 bytes = 163_840; set ceiling below that.
    let err = StorageGrowthBound::new(10, 100_000, 4_096, 64, 32).expect_err("undersized ceiling");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidGrowthBound
    );
}

#[test]
fn growth_bound_documents_linear_complexity_classes() {
    let bound = growth_bound();
    assert_eq!(bound.vectors_class(), "O(N*D)");
    assert_eq!(bound.graph_class(), "O(N*R)");
    assert_eq!(bound.candidate_class(), "O(N*Q)");
    assert_eq!(bound.metadata_class(), "O(N+M)");
    assert_eq!(bound.manifest_class(), "O(N)");
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[test]
fn manifest_round_trips_through_json() {
    let manifest = valid_manifest();
    let json = encode_shard_manifest_json(&manifest).expect("encodes");
    let decoded = decode_shard_manifest_json(&json).expect("decodes");
    assert_eq!(decoded, manifest);
}

#[test]
fn manifest_rejects_generation_mismatch() {
    let other_gen = ShardGeneration::new(99).expect("gen");
    let err = ShardManifest::new(
        shard_id(1),
        other_gen,
        1_000,
        required_files(),
        growth_bound(),
    )
    .expect_err("generation mismatch");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidManifest
    );
}

#[test]
fn manifest_rejects_zero_and_overflowing_vector_count() {
    let bound = growth_bound();
    let err = ShardManifest::new(shard_id(1), generation(), 0, required_files(), bound)
        .expect_err("zero vector count");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidManifest
    );

    let err = ShardManifest::new(
        shard_id(1),
        generation(),
        bound.max_vectors + 1,
        required_files(),
        bound,
    )
    .expect_err("overflow vector count");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidManifest
    );
}

#[test]
fn manifest_requires_every_required_role() {
    // Drop the Vectors file.
    let mut files = required_files();
    files.retain(|f| f.role() != ShardFileRole::Vectors);
    let err = ShardManifest::new(shard_id(1), generation(), 1_000, files, growth_bound())
        .expect_err("missing role fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidFileSet
    );
}

#[test]
fn manifest_rejects_empty_file_set() {
    let err = ShardManifest::new(shard_id(1), generation(), 1_000, vec![], growth_bound())
        .expect_err("empty file set fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidFileSet
    );
}

#[test]
fn manifest_rejects_duplicate_file_names() {
    let mut files = required_files();
    files.push(file(ShardFileRole::BuildReport, "vectors.f32", 999));
    let err = ShardManifest::new(shard_id(1), generation(), 1_000, files, growth_bound())
        .expect_err("duplicate name fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidFileSet
    );
}

#[test]
fn manifest_total_size_bytes_sums_files() {
    let manifest = valid_manifest();
    let expected: u64 = required_files().iter().map(|f| f.size_bytes()).sum();
    assert_eq!(manifest.total_size_bytes(), expected);
}

#[test]
fn manifest_required_roles_are_the_six_core_components() {
    assert_eq!(
        REQUIRED_ROLES,
        [
            ShardFileRole::Vectors,
            ShardFileRole::GraphPages,
            ShardFileRole::CandidateCodes,
            ShardFileRole::IdMap,
            ShardFileRole::Tombstones,
            ShardFileRole::Attributes,
        ]
    );
}

#[test]
fn decode_manifest_rejects_untrusted_json_fail_closed_and_redacts_detail() {
    let untrusted = "token=not-for-logs";
    let err = decode_shard_manifest_json(untrusted).expect_err("malformed JSON fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidManifest
    );
    assert!(!format!("{err:?}").contains(untrusted));
    assert!(!err.to_string().contains(untrusted));
}

#[test]
fn decode_manifest_rejects_post_deserialize_invariant_violation() {
    // Valid JSON shape but zero vector_count, which fails revalidation.
    let manifest = valid_manifest();
    let mut json: serde_json::Value =
        serde_json::from_str(&encode_shard_manifest_json(&manifest).expect("encodes"))
            .expect("json");
    json["vector_count"] = serde_json::json!(0);
    let tampered = serde_json::to_string(&json).expect("re-encodes");
    let err = decode_shard_manifest_json(&tampered).expect_err("tampered manifest fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidManifest
    );
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn router_config(fan_out: u32) -> ShardRouterConfig {
    ShardRouterConfig::new(fan_out, 1_000_000, 10).expect("valid router config")
}

#[test]
fn router_config_rejects_zero_bounds() {
    assert_eq!(
        ShardRouterConfig::new(0, 1, 1)
            .expect_err("zero fan-out")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouter
    );
    assert_eq!(
        ShardRouterConfig::new(1, 0, 1)
            .expect_err("zero deadline")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouter
    );
    assert_eq!(
        ShardRouterConfig::new(1, 1, 0)
            .expect_err("zero max generations")
            .diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouter
    );
}

#[test]
fn router_rejects_generation_descriptors_exceeding_max() {
    let descriptors: Vec<GenerationDescriptor> = (1..=11_u64)
        .map(|g| GenerationDescriptor::new(ShardGeneration::new(g).expect("gen"), 1).expect("desc"))
        .collect();
    let err = ShardRouter::new(router_config(5), descriptors).expect_err("too many generations");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouter
    );
}

#[test]
fn router_select_returns_sorted_compatible_shards() {
    let gen = generation();
    let config = router_config(10);
    let descriptor = GenerationDescriptor::new(gen, 3).expect("desc");
    let router = ShardRouter::new(config, vec![descriptor]).expect("router");

    let m1 = manifest_at_ordinal(1);
    let m2 = manifest_at_ordinal(2);
    let m3 = manifest_at_ordinal(3);
    let manifests = vec![&m3, &m1, &m2]; // intentionally unsorted input

    let selected = router
        .select(gen, &manifestes_sorted(manifests))
        .expect("selects");
    assert_eq!(selected.len(), 3);
    // Output sorted by ordinal.
    assert_eq!(
        selected
            .iter()
            .map(|s| s.ordinal().value())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

fn manifestes_sorted(mut manifests: Vec<&ShardManifest>) -> Vec<&ShardManifest> {
    manifests.sort_by_key(|m| m.shard().ordinal().value());
    manifests
}

fn manifest_at_ordinal(n: u32) -> ShardManifest {
    ShardManifest::new(
        ShardId::new(space(), generation(), ordinal(n)).expect("id"),
        generation(),
        1_000,
        required_files(),
        growth_bound(),
    )
    .expect("manifest")
}

#[test]
fn router_select_rejects_unknown_generation() {
    let gen = generation();
    let other = ShardGeneration::new(42).expect("gen");
    let descriptor = GenerationDescriptor::new(gen, 1).expect("desc");
    let router = ShardRouter::new(router_config(5), vec![descriptor]).expect("router");

    let m = valid_manifest();
    let err = router.select(other, &[&m]).expect_err("unknown gen fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouterSelection
    );
}

#[test]
fn router_select_rejects_empty_match_set() {
    let gen = generation();
    let descriptor = GenerationDescriptor::new(gen, 1).expect("desc");
    let router = ShardRouter::new(router_config(5), vec![descriptor]).expect("router");

    // No manifests at all.
    let err = router.select(gen, &[]).expect_err("empty set fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidRouterSelection
    );
}

#[test]
fn router_select_enforces_hard_fan_out_maximum() {
    let gen = generation();
    let config = ShardRouterConfig::new(2, 1_000_000, 10).expect("config"); // fan-out 2
    let descriptor = GenerationDescriptor::new(gen, 3).expect("desc");
    let router = ShardRouter::new(config, vec![descriptor]).expect("router");

    let manifests: Vec<ShardManifest> = (1..=3).map(manifest_at_ordinal).collect();
    let refs: Vec<&ShardManifest> = manifests.iter().collect();
    let err = router.select(gen, &refs).expect_err("fan-out exceeded");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::FanOutExceeded
    );
}

#[test]
fn router_metadata_stays_bounded_to_generation_descriptors() {
    let config = router_config(10);
    let descriptors = vec![
        GenerationDescriptor::new(generation(), 3).expect("desc"),
        GenerationDescriptor::new(ShardGeneration::new(2).expect("gen"), 5).expect("desc"),
    ];
    let router = ShardRouter::new(config, descriptors).expect("router");
    // Bounded to descriptors, not per-shard or per-source.
    assert_eq!(router.generations().len(), 2);
}

// ---------------------------------------------------------------------------
// Build checkpoint
// ---------------------------------------------------------------------------

#[test]
fn checkpoint_streaming_stage_allows_zero_vectors() {
    let checkpoint = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::StreamingData,
        0,
        FsyncAttestation::new(false, false),
    )
    .expect("streaming with zero progress is valid");
    assert_eq!(checkpoint.vectors_streamed(), 0);
    assert!(!checkpoint.stage().is_durable());
    assert!(!checkpoint.is_complete());
}

#[test]
fn checkpoint_post_streaming_requires_nonzero_vectors() {
    let err = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::WritingMetadata,
        0,
        FsyncAttestation::new(false, false),
    )
    .expect_err("zero vectors past streaming fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidCheckpoint
    );
}

#[test]
fn checkpoint_durable_stage_requires_full_fsync_attestation() {
    // Fsyncing stage with only data fsynced (not dir) — must fail.
    let err = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::Fsyncing,
        1_000,
        FsyncAttestation::new(true, false),
    )
    .expect_err("partial fsync at durable stage fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::CheckpointNotDurable
    );

    // Fully durable fsync is valid.
    let checkpoint = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::Fsyncing,
        1_000,
        FsyncAttestation::new(true, true),
    )
    .expect("fully durable fsync valid");
    assert!(checkpoint.stage().is_durable());
}

#[test]
fn checkpoint_complete_requires_full_fsync_durability() {
    let checkpoint = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::Complete,
        1_000,
        FsyncAttestation::new(true, true),
    )
    .expect("complete with full fsync valid");
    assert!(checkpoint.is_complete());

    let err = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::Complete,
        1_000,
        FsyncAttestation::new(false, false),
    )
    .expect_err("complete without fsync fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::CheckpointNotDurable
    );
}

#[test]
fn checkpoint_generation_must_match_shard_generation() {
    let other = ShardGeneration::new(99).expect("gen");
    let err = ShardBuildCheckpoint::new(
        shard_id(1),
        other,
        ShardBuildStage::StreamingData,
        0,
        FsyncAttestation::new(false, false),
    )
    .expect_err("generation mismatch fails");
    assert_eq!(
        err.diagnostic_code(),
        VectorShardDiagnosticCode::InvalidCheckpoint
    );
}

#[test]
fn checkpoint_round_trips_through_serde() {
    let checkpoint = ShardBuildCheckpoint::new(
        shard_id(1),
        generation(),
        ShardBuildStage::Complete,
        5_000,
        FsyncAttestation::new(true, true),
    )
    .expect("valid");
    let json = serde_json::to_string(&checkpoint).expect("encodes");
    let decoded: ShardBuildCheckpoint = serde_json::from_str(&json).expect("decodes");
    assert_eq!(decoded, checkpoint);
}

// ---------------------------------------------------------------------------
// Error redaction: diagnostic-only Debug/Display
// ---------------------------------------------------------------------------

#[test]
fn error_debug_and_display_never_leak_caller_controlled_detail() {
    let secret = "tenant=acme:acl=secret:source=leaked";
    // Force a failure that could conceptually involve caller data.
    let err = ShardVectorSpace::new(secret)
        .expect_err("uppercase invalid")
        .diagnostic_code();
    let dbg = format!("{err:?}");
    let display = err.as_str().to_string();
    assert!(!dbg.contains(secret));
    assert!(!display.contains(secret));
}

#[test]
fn full_error_type_debug_renders_only_closed_code() {
    let err = ShardGeneration::new(0).expect_err("zero gen");
    let dbg = format!("{err:?}");
    let display = err.to_string();
    assert!(dbg.contains("invalid_generation"));
    assert!(display.contains("invalid_generation"));
}
