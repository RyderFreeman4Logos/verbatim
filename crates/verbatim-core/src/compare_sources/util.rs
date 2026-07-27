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
    require_non_empty(field, value)
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
