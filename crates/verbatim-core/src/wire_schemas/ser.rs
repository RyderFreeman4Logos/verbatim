//! Deterministic serialization and content-hash helpers for wire documents.

use anyhow::{bail, Result};
use serde::{de::DeserializeOwned, Serialize};

use crate::types::hex_sha256;

/// Encode a wire document to compact JSON bytes.
///
/// Field order follows serde's struct declaration order; callers that need
/// golden stability must keep field declaration order stable and avoid maps
/// with non-deterministic iteration unless they use ordered maps.
pub fn encode_wire_document<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

/// Encode a wire document to pretty-printed JSON bytes (for fixtures/docs).
pub fn encode_wire_document_pretty<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(value)?)
}

/// Decode a wire document from JSON bytes without schema validation.
///
/// Prefer kind-specific `decode_*_json` helpers that fail closed on unknown
/// schema versions.
pub fn decode_wire_document<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Hex SHA-256 of the provided canonical body bytes.
pub fn wire_content_hash(body_bytes: &[u8]) -> String {
    hex_sha256(body_bytes)
}

/// Validate that a declared content hash matches body bytes.
pub fn verify_content_hash(declared: &str, body_bytes: &[u8]) -> Result<()> {
    if declared.trim().is_empty() {
        bail!("content hash must not be empty");
    }
    let actual = wire_content_hash(body_bytes);
    if declared != actual {
        bail!("content hash mismatch: declared {declared}, actual {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Tiny {
        a: u32,
        b: String,
    }

    #[test]
    fn compact_roundtrip_and_hash_stable() {
        let value = Tiny {
            a: 1,
            b: "x".into(),
        };
        let bytes = encode_wire_document(&value).unwrap();
        let back: Tiny = decode_wire_document(&bytes).unwrap();
        assert_eq!(back, value);
        let h1 = wire_content_hash(&bytes);
        let h2 = wire_content_hash(&encode_wire_document(&value).unwrap());
        assert_eq!(h1, h2);
        verify_content_hash(&h1, &bytes).unwrap();
        assert!(verify_content_hash("deadbeef", &bytes).is_err());
    }
}
