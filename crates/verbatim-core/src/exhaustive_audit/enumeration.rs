//! Candidate enumeration and deduplication contract types.

use serde::{Deserialize, Serialize};

use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::util::{require_digest, require_non_empty};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnumerationMethod {
    Exact,
    Metadata,
    Lexical,
    DenseAnn,
    Graph,
    TopK,
}

impl EnumerationMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Metadata => "metadata",
            Self::Lexical => "lexical",
            Self::DenseAnn => "dense_ann",
            Self::Graph => "graph",
            Self::TopK => "top_k",
        }
    }

    pub fn is_primary(self) -> bool {
        matches!(self, Self::Exact | Self::Metadata | Self::Lexical)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEnumeration {
    pub enumeration_id: String,
    pub scope_hash: String,
    pub method: EnumerationMethod,
    pub query_fingerprint: String,
    pub candidate_ids: Vec<String>,
    pub deterministic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEnumerationFields {
    pub enumeration_id: String,
    pub scope_hash: String,
    pub method: EnumerationMethod,
    pub query_fingerprint: String,
    pub candidate_ids: Vec<String>,
    pub deterministic: bool,
}

impl CandidateEnumeration {
    pub fn new(fields: CandidateEnumerationFields) -> ExhaustiveAuditResult<Self> {
        let enumeration = Self {
            enumeration_id: fields.enumeration_id,
            scope_hash: fields.scope_hash,
            method: fields.method,
            query_fingerprint: fields.query_fingerprint,
            candidate_ids: fields.candidate_ids,
            deterministic: fields.deterministic,
        };
        enumeration.validate()?;
        Ok(enumeration)
    }

    pub fn is_primary(&self) -> bool {
        self.method.is_primary()
    }

    pub fn is_deterministic_primary(&self) -> bool {
        self.is_primary() && self.deterministic
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        require_non_empty("enumeration_id", &self.enumeration_id)?;
        require_digest("enumeration.scope_hash", &self.scope_hash)?;
        require_digest("query_fingerprint", &self.query_fingerprint)?;
        let mut ids = std::collections::BTreeSet::new();
        for candidate_id in &self.candidate_ids {
            require_non_empty("candidate_id", candidate_id)?;
            if !ids.insert(candidate_id) {
                return Err(ExhaustiveAuditError::validation(
                    "candidate enumeration must not duplicate candidate ids",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateOccurrence {
    pub candidate_id: String,
    pub version_id: String,
    pub locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeduplicatedCandidate {
    pub canonical_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_duplicate_key: Option<String>,
    pub occurrences: Vec<CandidateOccurrence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeduplicatedCandidateFields {
    pub canonical_id: String,
    pub near_duplicate_key: Option<String>,
    pub occurrences: Vec<CandidateOccurrence>,
}

impl DeduplicatedCandidate {
    pub fn new(fields: DeduplicatedCandidateFields) -> ExhaustiveAuditResult<Self> {
        let candidate = Self {
            canonical_id: fields.canonical_id,
            near_duplicate_key: fields.near_duplicate_key,
            occurrences: fields.occurrences,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn occurrence_count(&self) -> usize {
        self.occurrences.len()
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        require_non_empty("canonical_id", &self.canonical_id)?;
        if let Some(key) = &self.near_duplicate_key {
            require_non_empty("near_duplicate_key", key)?;
        }
        if self.occurrences.is_empty() {
            return Err(ExhaustiveAuditError::validation(
                "deduplicated candidate requires at least one occurrence",
            ));
        }
        for occurrence in &self.occurrences {
            require_non_empty("occurrence.candidate_id", &occurrence.candidate_id)?;
            require_non_empty("occurrence.version_id", &occurrence.version_id)?;
            require_non_empty("occurrence.locator", &occurrence.locator)?;
        }
        Ok(())
    }
}
