//! Shared validation helpers for multi-hop research contract types.

use super::error::{ResearchError, ResearchResult};

pub(crate) fn require_non_empty(field: &str, value: &str) -> ResearchResult<()> {
    if value.trim().is_empty() {
        return Err(ResearchError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

pub(crate) fn require_digest(field: &str, value: &str) -> ResearchResult<()> {
    require_non_empty(field, value)?;
    if value.chars().any(|c| c.is_whitespace()) {
        return Err(ResearchError::validation(format!(
            "{field} must not contain whitespace"
        )));
    }
    Ok(())
}

pub(crate) fn require_positive_u32(field: &str, value: u32) -> ResearchResult<()> {
    if value == 0 {
        return Err(ResearchError::validation(format!("{field} must be >= 1")));
    }
    Ok(())
}

pub(crate) fn require_positive_u64(field: &str, value: u64) -> ResearchResult<()> {
    if value == 0 {
        return Err(ResearchError::validation(format!("{field} must be >= 1")));
    }
    Ok(())
}
