use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Reserved namespace for derived GraphRAG report artifacts.
pub const REPORT_ARTIFACT_ID_PREFIX: &str = "graphrag://report/";

/// True when `value` is a canonical or legacy derived report artifact id.
///
/// Both the canonical `graphrag://report/` namespace and the pre-#453 legacy
/// `graphrag:report:` prefix are reserved; neither may resolve as evidence.
pub fn is_report_artifact_id(value: &str) -> bool {
    value.starts_with(REPORT_ARTIFACT_ID_PREFIX) || value.starts_with("graphrag:report:")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportArtifactIdError {
    value: String,
}

impl ReportArtifactIdError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for ReportArtifactIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "community id must be non-empty and free of '/' and ':' characters: {}",
            self.value
        )
    }
}

impl Error for ReportArtifactIdError {}

/// Typed identity for a derived GraphRAG report artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReportArtifactId(String);

impl ReportArtifactId {
    /// Parse a canonical `graphrag://report/` or legacy `graphrag:report:` id.
    pub fn parse(value: &str) -> Result<Self, ReportArtifactIdError> {
        if !is_report_artifact_id(value) {
            return Err(ReportArtifactIdError::new(value));
        }
        let community_id = value
            .strip_prefix(REPORT_ARTIFACT_ID_PREFIX)
            .or_else(|| value.strip_prefix("graphrag:report:"))
            .unwrap_or(value);
        Self::new(community_id)
    }

    /// Build the canonical artifact id for `community_id`.
    pub fn new(community_id: &str) -> Result<Self, ReportArtifactIdError> {
        let community_id = community_id.trim();
        if community_id.is_empty() || community_id.contains('/') || community_id.contains(':') {
            return Err(ReportArtifactIdError::new(community_id));
        }
        Ok(Self(format!("{REPORT_ARTIFACT_ID_PREFIX}{community_id}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
