//! LanceDB FTS remains comparison-only until the complete #380 lexical conformance gate passes.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalOwnership {
    TantivyPrimary,
    LanceDbFtsComparisonOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalConformanceFlag {
    NotClaimed,
    SuitePassedIssue380,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbLexicalPolicy {
    ownership: LexicalOwnership,
    conformance: LexicalConformanceFlag,
    fts_comparison_enabled: bool,
}

impl<'de> Deserialize<'de> for LanceDbLexicalPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            ownership: LexicalOwnership,
            conformance: LexicalConformanceFlag,
            fts_comparison_enabled: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.ownership,
            wire.conformance,
            wire.fts_comparison_enabled,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LanceDbLexicalPolicy {
    pub fn new(
        ownership: LexicalOwnership,
        conformance: LexicalConformanceFlag,
        fts_comparison_enabled: bool,
    ) -> LanceDbBackendResult<Self> {
        Ok(Self {
            ownership,
            conformance,
            fts_comparison_enabled,
        })
    }

    pub const fn is_comparison_only(&self) -> bool {
        matches!(self.ownership, LexicalOwnership::LanceDbFtsComparisonOnly)
            || matches!(self.conformance, LexicalConformanceFlag::NotClaimed)
    }

    pub fn claim_lancedb_as_canonical(&self) -> LanceDbBackendResult<()> {
        if self.conformance != LexicalConformanceFlag::SuitePassedIssue380 {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::LexicalConformanceRequired,
            ));
        }
        Ok(())
    }

    pub const fn ownership(&self) -> LexicalOwnership {
        self.ownership
    }

    pub const fn fts_comparison_enabled(&self) -> bool {
        self.fts_comparison_enabled
    }
}
