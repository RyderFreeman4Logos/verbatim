use super::error::{ComparisonError, ComparisonResultType};

pub fn require_non_empty(field: &str, value: &str) -> ComparisonResultType<()> {
    if value.trim().is_empty() {
        return Err(ComparisonError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

pub fn require_digest(field: &str, value: &str) -> ComparisonResultType<()> {
    require_non_empty(field, value)?;
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(ComparisonError::validation(format!(
            "{field} must be a sha256: prefixed 64-character hexadecimal digest"
        )));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ComparisonError::validation(format!(
            "{field} must be a sha256: prefixed 64-character hexadecimal digest"
        )));
    }
    Ok(())
}

pub fn require_unique_non_empty(field: &str, values: &[String]) -> ComparisonResultType<()> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        require_non_empty(field, value)?;
        if !seen.insert(value) {
            return Err(ComparisonError::validation(format!(
                "{field} must not contain duplicates"
            )));
        }
    }
    Ok(())
}
