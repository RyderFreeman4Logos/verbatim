//! Sparse BM25 / hybrid caveats: Tantivy remains the primary lexical engine.

use serde::{Deserialize, Serialize};

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

/// Lexical ownership claim for a deployment profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalOwnership {
    /// Tantivy is the canonical BM25 / lexical backend.
    TantivyPrimary,
    /// Qdrant sparse/BM25 is control-only instrumentation, not a public replacement.
    QdrantSparseControlOnly,
}

/// Conformance flag for the #380 lexical suite. Absent unless the suite passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalConformanceFlag {
    NotClaimed,
    SuitePassedIssue380,
}

/// Validated lexical policy for Qdrant hybrid / BM25 control surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantLexicalPolicy {
    ownership: LexicalOwnership,
    conformance: LexicalConformanceFlag,
    hybrid_control_enabled: bool,
}

impl<'de> Deserialize<'de> for QdrantLexicalPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            ownership: LexicalOwnership,
            conformance: LexicalConformanceFlag,
            hybrid_control_enabled: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.ownership,
            wire.conformance,
            wire.hybrid_control_enabled,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl QdrantLexicalPolicy {
    pub fn new(
        ownership: LexicalOwnership,
        conformance: LexicalConformanceFlag,
        hybrid_control_enabled: bool,
    ) -> QdrantBackendResult<Self> {
        Ok(Self {
            ownership,
            conformance,
            hybrid_control_enabled,
        })
    }

    /// Returns true only when Qdrant sparse/BM25 is explicitly control-only.
    pub const fn is_control_only(&self) -> bool {
        matches!(self.ownership, LexicalOwnership::QdrantSparseControlOnly)
            || matches!(self.conformance, LexicalConformanceFlag::NotClaimed)
    }

    /// Rejects any claim that Qdrant BM25/hybrid replaces Tantivy without conformance.
    pub fn claim_tantivy_replacement(&self) -> QdrantBackendResult<()> {
        if self.conformance != LexicalConformanceFlag::SuitePassedIssue380
            || self.ownership != LexicalOwnership::TantivyPrimary
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::LexicalConformanceRequired,
            ));
        }
        Ok(())
    }

    pub const fn ownership(&self) -> LexicalOwnership {
        self.ownership
    }

    pub const fn conformance(&self) -> LexicalConformanceFlag {
        self.conformance
    }

    pub const fn hybrid_control_enabled(&self) -> bool {
        self.hybrid_control_enabled
    }
}
