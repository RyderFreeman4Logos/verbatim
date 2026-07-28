//! Serializable contract encode/decode helpers for immutable vector shards.
//!
//! Decode helpers revalidate decoded manifests before returning them, so
//! untrusted JSON cannot bypass constructor validation. Errors remain
//! diagnostic-code-only with no caller-controlled detail in `Debug` or `Display`.

use super::manifest::ShardManifest;
use super::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};

/// Encodes a validated manifest as JSON.
pub fn encode_shard_manifest_json(manifest: &ShardManifest) -> VectorShardResult<String> {
    manifest.validate()?;
    serde_json::to_string(manifest)
        .map_err(|_| VectorShardError::contract(VectorShardDiagnosticCode::SerializationFailed))
}

/// Decodes and revalidates a manifest from untrusted JSON.
pub fn decode_shard_manifest_json(input: &str) -> VectorShardResult<ShardManifest> {
    let manifest: ShardManifest = serde_json::from_str(input)
        .map_err(|_| VectorShardError::contract(VectorShardDiagnosticCode::InvalidManifest))?;
    manifest
        .validate()
        .map_err(|_| VectorShardError::contract(VectorShardDiagnosticCode::InvalidManifest))?;
    Ok(manifest)
}
