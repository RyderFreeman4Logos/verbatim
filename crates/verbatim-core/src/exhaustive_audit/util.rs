use serde::Serialize;
use sha2::{Digest, Sha256};

use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};

pub(crate) fn require_non_empty(field: &str, value: &str) -> ExhaustiveAuditResult<()> {
    if value.trim().is_empty() {
        return Err(ExhaustiveAuditError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

pub(crate) fn require_digest(field: &str, value: &str) -> ExhaustiveAuditResult<()> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(ExhaustiveAuditError::validation(format!(
            "{field} must be a sha256 digest"
        )));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExhaustiveAuditError::validation(format!(
            "{field} must be a sha256 digest"
        )));
    }
    Ok(())
}

pub fn content_hash_of<T: Serialize>(value: &T) -> ExhaustiveAuditResult<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| ExhaustiveAuditError::validation(format!("canonical JSON: {err}")))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}
