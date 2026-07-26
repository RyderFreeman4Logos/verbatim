//! Opaque snapshot-bound cursor claims, sealing, and open/validate helpers.
//!
//! Wire form is intentionally opaque: `v1.<base64url(claims_json)>.<hex_seal>`.
//! The seal is a keyed content hash (HMAC-style SHA-256) over the claims bytes.
//! Tamper, principal mismatch, generation mismatch, profile/policy drift, mode
//! swap, and expiry all fail closed with [`super::error::CursorError`].

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::index_publication::{
    PointerEpoch, QueryPublicationBinding, QueryPublicationBindingKind,
};
use crate::storage_ports::{PageCursor, StorageError, StorageGeneration, StorageResult};

use super::error::{CursorError, CursorResult};
use super::page::PaginationMode;

/// Wire schema for sealed cursor claims. Unknown versions fail closed.
pub const CURSOR_SCHEMA_VERSION: u32 = 1;

const CURSOR_WIRE_PREFIX: &str = "v1";

/// Server-held sealing key material. Never placed on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorSealKey(Vec<u8>);

impl CursorSealKey {
    pub fn new(bytes: impl Into<Vec<u8>>) -> StorageResult<Self> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(StorageError::invalid_request(
                "cursor seal key must not be empty",
            ));
        }
        if bytes.len() > 1024 {
            return Err(StorageError::invalid_request(
                "cursor seal key exceeds 1024 bytes",
            ));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Field bundle for [`CursorClaims::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorClaimsFields {
    pub mode: PaginationMode,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_generation: StorageGeneration,
    pub profile_ref: String,
    pub policy_version: String,
    /// Last stable keyset / sort key (score + id tie-break, or exhaustive id).
    pub last_sort_key: String,
    /// Page ordinal for diagnostics (0-based page that produced this cursor).
    pub page_ordinal: u32,
    pub expires_at_unix: u64,
    pub pointer_epoch: Option<PointerEpoch>,
    /// Opaque consumer correlation id embedded in the publication binding.
    pub consumer_id: String,
}

/// Canonical claims sealed into an opaque cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorClaims {
    pub schema_version: u32,
    pub mode: PaginationMode,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_binding: QueryPublicationBinding,
    pub profile_ref: String,
    pub policy_version: String,
    pub last_sort_key: String,
    pub page_ordinal: u32,
    pub expires_at_unix: u64,
}

impl CursorClaims {
    pub fn new(fields: CursorClaimsFields) -> StorageResult<Self> {
        let mut binding = QueryPublicationBinding::new(
            QueryPublicationBindingKind::Cursor,
            fields.publication_generation,
            fields.consumer_id,
        )?;
        if let Some(epoch) = fields.pointer_epoch {
            binding = binding.with_pointer_epoch(epoch);
        }
        let claims = Self {
            schema_version: CURSOR_SCHEMA_VERSION,
            mode: fields.mode,
            query_plan_hash: fields.query_plan_hash,
            principal: fields.principal,
            publication_binding: binding,
            profile_ref: fields.profile_ref,
            policy_version: fields.policy_version,
            last_sort_key: fields.last_sort_key,
            page_ordinal: fields.page_ordinal,
            expires_at_unix: fields.expires_at_unix,
        };
        claims.validate_structure()?;
        Ok(claims)
    }

    pub fn validate_structure(&self) -> StorageResult<()> {
        if self.schema_version == 0 {
            return Err(StorageError::invalid_request(
                "cursor schema_version must be > 0",
            ));
        }
        if self.schema_version != CURSOR_SCHEMA_VERSION {
            return Err(StorageError::invalid_request(format!(
                "unsupported cursor schema_version {}; expected {CURSOR_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        require_non_empty("query_plan_hash", &self.query_plan_hash)?;
        require_non_empty("principal", &self.principal)?;
        require_non_empty("profile_ref", &self.profile_ref)?;
        require_non_empty("policy_version", &self.policy_version)?;
        require_non_empty("last_sort_key", &self.last_sort_key)?;
        if self.publication_binding.kind != QueryPublicationBindingKind::Cursor {
            return Err(StorageError::invalid_request(
                "cursor publication binding kind must be cursor",
            ));
        }
        self.publication_binding.validate()?;
        Ok(())
    }

    pub fn publication_generation(&self) -> StorageGeneration {
        self.publication_binding.publication_generation
    }
}

/// Expected continuation context supplied by the caller on page N+1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationContext {
    pub mode: PaginationMode,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_generation: StorageGeneration,
    pub profile_ref: String,
    pub policy_version: String,
    /// Optional: active generation still readable for this binding.
    pub available_generation: Option<StorageGeneration>,
    pub now_unix: u64,
}

/// Seal claims into an opaque [`PageCursor`].
pub fn seal_cursor(claims: &CursorClaims, key: &CursorSealKey) -> CursorResult<PageCursor> {
    claims
        .validate_structure()
        .map_err(|err| CursorError::invalid(err.to_string()))?;
    let payload = encode_claims_json(claims)?;
    let seal = compute_seal(key.as_bytes(), &payload);
    let body = URL_SAFE_NO_PAD.encode(payload);
    let wire = format!("{CURSOR_WIRE_PREFIX}.{body}.{seal}");
    PageCursor::new(wire).map_err(|err| CursorError::invalid(err.to_string()))
}

/// Encode claims to an opaque cursor (alias of [`seal_cursor`]).
pub fn encode_cursor(claims: &CursorClaims, key: &CursorSealKey) -> CursorResult<PageCursor> {
    seal_cursor(claims, key)
}

/// Open and integrity-check an opaque cursor. Does **not** check expiry or
/// continuation binding — use [`validate_cursor_continuation`] for that.
pub fn open_cursor(cursor: &PageCursor, key: &CursorSealKey) -> CursorResult<CursorClaims> {
    let wire = cursor.0.trim();
    if wire.is_empty() {
        return Err(CursorError::invalid("cursor is empty"));
    }
    let parts: Vec<&str> = wire.split('.').collect();
    if parts.len() != 3 {
        return Err(CursorError::invalid(
            "cursor wire form must be v1.<payload>.<seal>",
        ));
    }
    if parts[0] != CURSOR_WIRE_PREFIX {
        return Err(CursorError::invalid(format!(
            "unsupported cursor wire prefix {}; expected {CURSOR_WIRE_PREFIX}",
            parts[0]
        )));
    }
    let payload = URL_SAFE_NO_PAD
        .decode(parts[1].as_bytes())
        .map_err(|err| CursorError::invalid(format!("cursor payload base64: {err}")))?;
    let expected_seal = compute_seal(key.as_bytes(), &payload);
    if !constant_time_eq(expected_seal.as_bytes(), parts[2].as_bytes()) {
        return Err(CursorError::invalid(
            "cursor seal mismatch (tamper or wrong key)",
        ));
    }
    decode_cursor_claims(&payload)
}

/// Decode claims JSON after integrity verification (or for golden fixtures).
pub fn decode_cursor_claims(bytes: &[u8]) -> CursorResult<CursorClaims> {
    let claims: CursorClaims = serde_json::from_slice(bytes)
        .map_err(|err| CursorError::invalid(format!("cursor claims decode: {err}")))?;
    claims
        .validate_structure()
        .map_err(|err| CursorError::invalid(err.to_string()))?;
    Ok(claims)
}

/// Fail closed when claims do not match the continuation request context.
pub fn validate_cursor_continuation(
    claims: &CursorClaims,
    ctx: &ContinuationContext,
) -> CursorResult<()> {
    if ctx.now_unix > claims.expires_at_unix {
        return Err(CursorError::expired(claims.expires_at_unix, ctx.now_unix));
    }
    if claims.mode != ctx.mode {
        return Err(CursorError::mode_mismatch(claims.mode, ctx.mode));
    }
    if claims.principal != ctx.principal {
        return Err(CursorError::unauthorized(
            "cursor principal does not match authenticated caller",
        ));
    }
    if claims.query_plan_hash != ctx.query_plan_hash {
        return Err(CursorError::query_mismatch(
            &claims.query_plan_hash,
            &ctx.query_plan_hash,
        ));
    }
    if claims.profile_ref != ctx.profile_ref {
        return Err(CursorError::profile_changed(
            &claims.profile_ref,
            &ctx.profile_ref,
        ));
    }
    if claims.policy_version != ctx.policy_version {
        return Err(CursorError::policy_changed(
            &claims.policy_version,
            &ctx.policy_version,
        ));
    }
    let bound = claims.publication_generation();
    // Request binding ≠ sealed cursor generation is a binding mismatch, not
    // "generation gone". Do not project request gen into StaleGeneration.actual.
    if bound != ctx.publication_generation {
        return Err(CursorError::generation_mismatch(
            bound,
            ctx.publication_generation,
            "cursor publication generation does not match request binding",
        ));
    }
    // True unavailability: bound generation no longer readable / not retained.
    if let Some(available) = ctx.available_generation {
        if available != bound {
            return Err(CursorError::generation_gone(
                bound,
                Some(available),
                "bound publication generation is no longer available",
            ));
        }
    }
    Ok(())
}

fn encode_claims_json(claims: &CursorClaims) -> CursorResult<Vec<u8>> {
    // Compact, field-order-stable JSON via serde_json Value sort is not required
    // because the seal covers the exact sealed bytes; re-open uses those bytes.
    serde_json::to_vec(claims)
        .map_err(|err| CursorError::invalid(format!("cursor claims encode: {err}")))
}

/// Keyed SHA-256 seal: SHA256(key || 0x00 || payload). Walking-skeleton MAC.
fn compute_seal(key: &[u8], payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key);
    hasher.update([0u8]);
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn require_non_empty(field: &str, value: &str) -> StorageResult<()> {
    if value.trim().is_empty() {
        return Err(StorageError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}
