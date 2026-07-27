//! Small deterministic helpers shared by citation-audit contract types.

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{CitationAuditError, CitationAuditResult};

pub fn require_non_empty(field: &str, value: &str) -> CitationAuditResult<()> {
    if value.trim().is_empty() {
        return Err(CitationAuditError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

pub fn require_sha256(field: &str, value: &str) -> CitationAuditResult<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CitationAuditError::validation(format!(
            "{field} must be a 64-character SHA-256 hexadecimal digest"
        )));
    }
    Ok(())
}

pub fn content_hash_of<T: Serialize + ?Sized>(value: &T) -> CitationAuditResult<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| CitationAuditError::validation("artifact cannot be serialized for hashing"))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}
