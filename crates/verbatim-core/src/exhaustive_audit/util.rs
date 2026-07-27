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
    require_non_empty(field, value)?;
    if value.chars().any(char::is_whitespace) {
        return Err(ExhaustiveAuditError::validation(format!(
            "{field} must not contain whitespace"
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
