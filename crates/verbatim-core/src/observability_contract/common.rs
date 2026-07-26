//! Shared validation helpers for the observability contract.

use anyhow::{bail, Result};

use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

pub(super) fn validate_schema_version(schema_version: u32) -> Result<()> {
    if schema_version != OBSERVABILITY_CONTRACT_SCHEMA_VERSION {
        bail!(
            "unsupported observability contract schema version {schema_version}; expected {OBSERVABILITY_CONTRACT_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

pub(super) fn require_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(())
}

pub(super) fn require_error_class(error_class: Option<&str>) -> Result<()> {
    match error_class {
        Some(class) if !class.trim().is_empty() => Ok(()),
        _ => bail!("error spans require non-empty error_class"),
    }
}

pub(super) fn ensure_end_after_start(start_unix_ms: u64, end_unix_ms: u64) -> Result<()> {
    if end_unix_ms < start_unix_ms {
        bail!("span end_unix_ms ({end_unix_ms}) is before start_unix_ms ({start_unix_ms})");
    }
    Ok(())
}
