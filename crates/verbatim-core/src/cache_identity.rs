//! Cache identity contract with authorization isolation (CACHE-001 / issue #339).
//!
//! This module is the first walking skeleton for cache correctness: a typed,
//! testable specification of what every cache key must include so that hits
//! cannot cross principals, ACL scopes, query plans, source generations, model
//! fingerprints, trust domains, policy versions, or ContextPack hashes.
//!
//! Residual (not in this slice): wiring existing embedding/retrieval/answer/
//! graph/provider caches to adopt the key, remote tombstone propagation,
//! storage TTL/encryption bounds, trust-domain invalidation events, and closing
//! epic #339. See `docs/architecture/cache-identity.md`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::types::hex_sha256;

/// Schema version for [`CacheIdentity`], [`CacheKey`], and related wire forms.
///
/// Unknown versions must fail closed on decode rather than being silently
/// accepted as current-schema entries.
pub const CACHE_IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Canonical inputs that define the authorization and semantic scope of a cache
/// entry.
///
/// Shared reuse across entries is only valid when every field is equivalent.
/// Query text alone is never a sufficient key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheIdentity {
    /// Wire schema version. Must equal [`CACHE_IDENTITY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Authenticated principal (user, tenant, or equivalent identity string).
    pub principal: String,
    /// Authorization scope such as collection membership or classification.
    pub acl_scope: String,
    /// Deterministic hash of the QueryPlan and retrieval/embed profile inputs.
    pub query_plan_hash: String,
    /// Source/index generation marker fencing stale generations.
    pub source_generation: String,
    /// Served model version fingerprint (embedding, rerank, or generation).
    pub model_fingerprint: String,
    /// Trust classification for the entry's visibility domain.
    pub trust_domain: String,
    /// Cache policy / lifecycle / retention policy version.
    pub policy_version: String,
    /// Hash of the ContextPack (or equivalent grounded context payload).
    ///
    /// Answer / generation caches must include this so two responses for the
    /// same principal/query/ACL cannot share an entry across distinct packs.
    pub context_pack_hash: String,
}

/// Field bundle for [`CacheIdentity::new`] to keep the constructor arity small.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheIdentityFields {
    pub principal: String,
    pub acl_scope: String,
    pub query_plan_hash: String,
    pub source_generation: String,
    pub model_fingerprint: String,
    pub trust_domain: String,
    pub policy_version: String,
    pub context_pack_hash: String,
}

impl CacheIdentity {
    /// Build a current-schema identity from canonical field strings.
    pub fn new(fields: CacheIdentityFields) -> Self {
        Self {
            schema_version: CACHE_IDENTITY_SCHEMA_VERSION,
            principal: fields.principal,
            acl_scope: fields.acl_scope,
            query_plan_hash: fields.query_plan_hash,
            source_generation: fields.source_generation,
            model_fingerprint: fields.model_fingerprint,
            trust_domain: fields.trust_domain,
            policy_version: fields.policy_version,
            context_pack_hash: fields.context_pack_hash,
        }
    }

    /// Reject unknown or unsupported schema versions.
    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    /// Derive a content-addressed [`CacheKey`] when the schema is supported.
    pub fn to_cache_key(&self) -> Result<CacheKey> {
        self.validate_schema()?;
        Ok(CacheKey {
            schema_version: self.schema_version,
            digest: self.content_digest(),
            principal: self.principal.clone(),
            acl_scope: self.acl_scope.clone(),
            query_plan_hash: self.query_plan_hash.clone(),
            source_generation: self.source_generation.clone(),
            model_fingerprint: self.model_fingerprint.clone(),
            trust_domain: self.trust_domain.clone(),
            policy_version: self.policy_version.clone(),
            context_pack_hash: self.context_pack_hash.clone(),
        })
    }

    /// Stable SHA-256 digest over length-prefixed canonical field bytes.
    fn content_digest(&self) -> String {
        let mut payload = Vec::with_capacity(256);
        append_field(&mut payload, b"cache-identity-v1");
        append_u32(&mut payload, self.schema_version);
        append_field(&mut payload, self.principal.as_bytes());
        append_field(&mut payload, self.acl_scope.as_bytes());
        append_field(&mut payload, self.query_plan_hash.as_bytes());
        append_field(&mut payload, self.source_generation.as_bytes());
        append_field(&mut payload, self.model_fingerprint.as_bytes());
        append_field(&mut payload, self.trust_domain.as_bytes());
        append_field(&mut payload, self.policy_version.as_bytes());
        append_field(&mut payload, self.context_pack_hash.as_bytes());
        hex_sha256(&payload)
    }
}

/// Content-addressed cache key derived from [`CacheIdentity`].
///
/// The digest is the primary address. Matching fields are retained so
/// invalidation can target principal, ACL, generation, model, policy, or
/// ContextPack scope without re-materializing the original identity document.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub schema_version: u32,
    /// Hex-encoded SHA-256 of the canonical identity payload.
    pub digest: String,
    pub principal: String,
    pub acl_scope: String,
    pub query_plan_hash: String,
    pub source_generation: String,
    pub model_fingerprint: String,
    pub trust_domain: String,
    pub policy_version: String,
    pub context_pack_hash: String,
}

impl CacheKey {
    /// Reject unknown or unsupported schema versions.
    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }
}

/// Dependency set recorded beside a cache entry for invalidation lineage.
///
/// This does not expand the address key by itself; adapters should include the
/// generations/hashes that matter in [`CacheIdentity`] and list the concrete
/// artifacts here for multi-entry fan-out.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct CacheDependencyGraph {
    pub schema_version: u32,
    pub source_ids: Vec<String>,
    pub profile_ids: Vec<String>,
    pub graph_generations: Vec<String>,
    pub model_fingerprints: Vec<String>,
    pub acl_scopes: Vec<String>,
    pub policy_versions: Vec<String>,
}

impl CacheDependencyGraph {
    /// Empty dependency graph on the current schema version.
    pub fn new() -> Self {
        Self {
            schema_version: CACHE_IDENTITY_SCHEMA_VERSION,
            ..Self::default()
        }
    }

    /// Reject unknown or unsupported schema versions.
    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }
}

/// Events that must invalidate affected cache entries across layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidationEvent {
    /// Source content edit under a principal and generation fence.
    Edit {
        principal: String,
        source_generation: String,
    },
    /// Source deletion under a principal and generation fence.
    Delete {
        principal: String,
        source_generation: String,
    },
    /// Snapshot replacement under a principal and generation fence.
    Snapshot {
        principal: String,
        source_generation: String,
    },
    /// Authorization scope change for a principal.
    Acl {
        principal: String,
        acl_scope: String,
    },
    /// Lifecycle/hold/rights transition under a principal and generation fence.
    Lifecycle {
        principal: String,
        source_generation: String,
    },
    /// Served model identity changed.
    Model { model_fingerprint: String },
    /// Query/profile plan identity changed.
    Profile { query_plan_hash: String },
    /// Graph rebuild or graph generation change.
    Graph { graph_generation: String },
    /// Retention/policy version change.
    Retention { policy_version: String },
}

/// Return whether `key` must be treated as stale under `event`.
///
/// Matching is intentionally narrow and field-explicit so a deletion or ACL
/// change for one principal never invalidates another principal's entries.
pub fn cache_key_matches_invalidation(key: &CacheKey, event: &InvalidationEvent) -> bool {
    match event {
        InvalidationEvent::Edit {
            principal,
            source_generation,
        }
        | InvalidationEvent::Delete {
            principal,
            source_generation,
        }
        | InvalidationEvent::Snapshot {
            principal,
            source_generation,
        }
        | InvalidationEvent::Lifecycle {
            principal,
            source_generation,
        } => key.principal == *principal && key.source_generation == *source_generation,
        InvalidationEvent::Acl {
            principal,
            acl_scope,
        } => key.principal == *principal && key.acl_scope == *acl_scope,
        InvalidationEvent::Model { model_fingerprint } => {
            key.model_fingerprint == *model_fingerprint
        }
        InvalidationEvent::Profile { query_plan_hash } => key.query_plan_hash == *query_plan_hash,
        // Graph generation is fenced through the source/index generation marker
        // until dedicated graph fields are wired into keys.
        InvalidationEvent::Graph { graph_generation } => key.source_generation == *graph_generation,
        InvalidationEvent::Retention { policy_version } => key.policy_version == *policy_version,
    }
}

/// Decode a JSON [`CacheIdentity`] and reject unknown schema versions.
pub fn decode_cache_identity_json(bytes: &[u8]) -> Result<CacheIdentity> {
    let identity: CacheIdentity = serde_json::from_slice(bytes)?;
    identity.validate_schema()?;
    Ok(identity)
}

/// Decode a JSON [`CacheKey`] and reject unknown schema versions.
pub fn decode_cache_key_json(bytes: &[u8]) -> Result<CacheKey> {
    let key: CacheKey = serde_json::from_slice(bytes)?;
    key.validate_schema()?;
    Ok(key)
}

/// Decode a JSON [`CacheDependencyGraph`] and reject unknown schema versions.
pub fn decode_cache_dependency_graph_json(bytes: &[u8]) -> Result<CacheDependencyGraph> {
    let deps: CacheDependencyGraph = serde_json::from_slice(bytes)?;
    deps.validate_schema()?;
    Ok(deps)
}

fn validate_schema_version(schema_version: u32) -> Result<()> {
    if schema_version != CACHE_IDENTITY_SCHEMA_VERSION {
        bail!(
            "unsupported cache identity schema version {schema_version}; expected {CACHE_IDENTITY_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

fn append_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn append_field(buf: &mut Vec<u8>, field: &[u8]) {
    let len = u32::try_from(field.len()).unwrap_or(u32::MAX);
    append_u32(buf, len);
    buf.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> CacheIdentity {
        identity_with(
            "user-a",
            "collection:alpha",
            "plan-deadbeef",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-1",
        )
    }

    fn identity_with(
        principal: &str,
        acl_scope: &str,
        query_plan_hash: &str,
        source_generation: &str,
        model_fingerprint: &str,
        trust_domain: &str,
        policy_version: &str,
        context_pack_hash: &str,
    ) -> CacheIdentity {
        CacheIdentity::new(CacheIdentityFields {
            principal: principal.into(),
            acl_scope: acl_scope.into(),
            query_plan_hash: query_plan_hash.into(),
            source_generation: source_generation.into(),
            model_fingerprint: model_fingerprint.into(),
            trust_domain: trust_domain.into(),
            policy_version: policy_version.into(),
            context_pack_hash: context_pack_hash.into(),
        })
    }

    #[test]
    fn different_principals_same_query_produce_different_keys() {
        let left = identity_with(
            "user-a",
            "collection:alpha",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-1",
        );
        let right = identity_with(
            "user-b",
            "collection:alpha",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-1",
        );

        let left_key = left.to_cache_key().unwrap();
        let right_key = right.to_cache_key().unwrap();
        assert_ne!(left_key.digest, right_key.digest);
        assert_ne!(left_key, right_key);
    }

    #[test]
    fn different_acl_scopes_produce_different_keys() {
        let left = identity_with(
            "user-a",
            "collection:alpha",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-1",
        );
        let right = identity_with(
            "user-a",
            "collection:beta",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-1",
        );

        assert_ne!(
            left.to_cache_key().unwrap().digest,
            right.to_cache_key().unwrap().digest
        );
    }

    #[test]
    fn different_context_pack_hashes_produce_different_keys() {
        // Answer cache cannot cross ContextPack hash for the same principal,
        // query plan, and ACL scope.
        let left = identity_with(
            "user-a",
            "collection:alpha",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-aaa",
        );
        let right = identity_with(
            "user-a",
            "collection:alpha",
            "plan-same",
            "src-gen-1",
            "model-fp-1",
            "trust:internal",
            "policy-v1",
            "ctx-pack-bbb",
        );

        let left_key = left.to_cache_key().unwrap();
        let right_key = right.to_cache_key().unwrap();
        assert_ne!(left_key.digest, right_key.digest);
        assert_ne!(left_key.context_pack_hash, right_key.context_pack_hash);
    }

    #[test]
    fn content_digest_isolates_each_semantic_field() {
        // Each content_digest participant differing alone must change the key.
        let baseline = sample_identity();
        let cases = [
            identity_with(
                "user-b",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-1",
                "trust:internal",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:beta",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-1",
                "trust:internal",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-other",
                "src-gen-1",
                "model-fp-1",
                "trust:internal",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-2",
                "model-fp-1",
                "trust:internal",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-2",
                "trust:internal",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-1",
                "trust:external",
                "policy-v1",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-1",
                "trust:internal",
                "policy-v2",
                "ctx-pack-1",
            ),
            identity_with(
                "user-a",
                "collection:alpha",
                "plan-deadbeef",
                "src-gen-1",
                "model-fp-1",
                "trust:internal",
                "policy-v1",
                "ctx-pack-2",
            ),
        ];

        let baseline_digest = baseline.to_cache_key().unwrap().digest;
        for (idx, case) in cases.iter().enumerate() {
            assert_ne!(
                baseline_digest,
                case.to_cache_key().unwrap().digest,
                "case {idx} must isolate content_digest"
            );
        }
    }

    #[test]
    fn same_principal_scope_and_query_produce_identical_keys() {
        let first = sample_identity().to_cache_key().unwrap();
        let second = sample_identity().to_cache_key().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.digest.len(), 64);
    }

    #[test]
    fn deletion_invalidates_matching_key() {
        let key = sample_identity().to_cache_key().unwrap();
        let event = InvalidationEvent::Delete {
            principal: "user-a".into(),
            source_generation: "src-gen-1".into(),
        };
        assert!(cache_key_matches_invalidation(&key, &event));
    }

    #[test]
    fn deletion_for_different_principal_does_not_invalidate() {
        let key = sample_identity().to_cache_key().unwrap();
        let event = InvalidationEvent::Delete {
            principal: "user-b".into(),
            source_generation: "src-gen-1".into(),
        };
        assert!(!cache_key_matches_invalidation(&key, &event));
    }

    #[test]
    fn principal_scoped_invalidation_events_match_and_miss() {
        let key = sample_identity().to_cache_key().unwrap();
        let match_cases = [
            InvalidationEvent::Edit {
                principal: "user-a".into(),
                source_generation: "src-gen-1".into(),
            },
            InvalidationEvent::Snapshot {
                principal: "user-a".into(),
                source_generation: "src-gen-1".into(),
            },
            InvalidationEvent::Lifecycle {
                principal: "user-a".into(),
                source_generation: "src-gen-1".into(),
            },
        ];
        for event in &match_cases {
            assert!(
                cache_key_matches_invalidation(&key, event),
                "expected match for {event:?}"
            );
        }

        let miss_cases = [
            InvalidationEvent::Edit {
                principal: "user-b".into(),
                source_generation: "src-gen-1".into(),
            },
            InvalidationEvent::Snapshot {
                principal: "user-a".into(),
                source_generation: "src-gen-other".into(),
            },
            InvalidationEvent::Lifecycle {
                principal: "user-b".into(),
                source_generation: "src-gen-other".into(),
            },
        ];
        for event in &miss_cases {
            assert!(
                !cache_key_matches_invalidation(&key, event),
                "expected non-match for {event:?}"
            );
        }
    }

    #[test]
    fn profile_and_graph_invalidation_match_and_miss() {
        let key = sample_identity().to_cache_key().unwrap();
        assert!(cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Profile {
                query_plan_hash: "plan-deadbeef".into(),
            }
        ));
        assert!(!cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Profile {
                query_plan_hash: "plan-other".into(),
            }
        ));
        assert!(cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Graph {
                graph_generation: "src-gen-1".into(),
            }
        ));
        assert!(!cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Graph {
                graph_generation: "src-gen-other".into(),
            }
        ));
    }

    #[test]
    fn serialization_roundtrip_preserves_identity_and_key() {
        let identity = sample_identity();
        let key = identity.to_cache_key().unwrap();
        let deps = CacheDependencyGraph {
            schema_version: CACHE_IDENTITY_SCHEMA_VERSION,
            source_ids: vec!["src-1".into()],
            profile_ids: vec!["default".into()],
            graph_generations: vec!["graph-1".into()],
            model_fingerprints: vec!["model-fp-1".into()],
            acl_scopes: vec!["collection:alpha".into()],
            policy_versions: vec!["policy-v1".into()],
        };

        let identity_bytes = serde_json::to_vec(&identity).unwrap();
        let key_bytes = serde_json::to_vec(&key).unwrap();
        let deps_bytes = serde_json::to_vec(&deps).unwrap();

        let identity_back = decode_cache_identity_json(&identity_bytes).unwrap();
        let key_back = decode_cache_key_json(&key_bytes).unwrap();
        let deps_back = decode_cache_dependency_graph_json(&deps_bytes).unwrap();

        assert_eq!(identity_back, identity);
        assert_eq!(key_back, key);
        assert_eq!(deps_back, deps);

        let event = InvalidationEvent::Acl {
            principal: "user-a".into(),
            acl_scope: "collection:alpha".into(),
        };
        let event_bytes = serde_json::to_vec(&event).unwrap();
        let event_back: InvalidationEvent = serde_json::from_slice(&event_bytes).unwrap();
        assert_eq!(event_back, event);
        assert!(cache_key_matches_invalidation(&key, &event_back));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let mut identity = sample_identity();
        identity.schema_version = CACHE_IDENTITY_SCHEMA_VERSION + 1;
        let err = identity
            .to_cache_key()
            .expect_err("unknown schema must fail");
        assert!(err
            .to_string()
            .contains("unsupported cache identity schema"));

        let mut wire = sample_identity();
        wire.schema_version = 99;
        let bytes = serde_json::to_vec(&wire).unwrap();
        let decode_err = decode_cache_identity_json(&bytes).expect_err("decode must fail closed");
        assert!(decode_err
            .to_string()
            .contains("unsupported cache identity schema version 99"));

        let mut key = sample_identity().to_cache_key().unwrap();
        key.schema_version = 7;
        let key_bytes = serde_json::to_vec(&key).unwrap();
        let key_err = decode_cache_key_json(&key_bytes).expect_err("key decode must fail closed");
        assert!(key_err
            .to_string()
            .contains("unsupported cache identity schema version 7"));

        let mut deps = CacheDependencyGraph::new();
        deps.schema_version = 42;
        let deps_bytes = serde_json::to_vec(&deps).unwrap();
        let deps_err = decode_cache_dependency_graph_json(&deps_bytes)
            .expect_err("dependency graph decode must fail closed");
        assert!(deps_err
            .to_string()
            .contains("unsupported cache identity schema version 42"));
    }

    #[test]
    fn model_and_retention_invalidation_match_fields() {
        let key = sample_identity().to_cache_key().unwrap();
        assert!(cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Model {
                model_fingerprint: "model-fp-1".into(),
            }
        ));
        assert!(!cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Model {
                model_fingerprint: "model-fp-other".into(),
            }
        ));
        assert!(cache_key_matches_invalidation(
            &key,
            &InvalidationEvent::Retention {
                policy_version: "policy-v1".into(),
            }
        ));
    }
}
