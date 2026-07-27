//! Redaction-safe deletion proof and deterministic verification.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    DeletionPlan, DeletionTarget, ErasureDiagnosticCode, ErasureError, ErasureResult,
    ReconciliationReceipt,
};

/// Per-target externally safe status. It names only the backend class, never a
/// deleted source identifier, document body, excerpt, vector, or credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionOutcome {
    Confirmed,
    PendingRemoteReconciliation,
}

/// Explicit representation that the serialized proof intentionally contains no
/// deleted/restricted content or direct source identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofRedaction {
    ContentAndIdentifiersOmitted,
}

/// Verifiable report for an erasure request. `scope_commitment` is a SHA-256
/// commitment to request identifiers, not a copy of them; callers verify it
/// against a private plan instead of publishing restricted source metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionProof {
    pub schema_version: u32,
    pub scope_commitment: [u8; 32],
    pub source_count: u32,
    pub target_outcomes: BTreeMap<DeletionTarget, DeletionOutcome>,
    pub redaction: ProofRedaction,
}

impl DeletionProof {
    pub fn from_reconciliation(
        plan: &DeletionPlan,
        reconciliation: &ReconciliationReceipt,
    ) -> ErasureResult<Self> {
        plan.validate()?;
        reconciliation.validate(plan)?;
        let outcomes = plan
            .matrix
            .entries()
            .iter()
            .map(|entry| {
                let outcome = if reconciliation
                    .propagation
                    .pending_remote_targets
                    .contains(&entry.target)
                {
                    DeletionOutcome::PendingRemoteReconciliation
                } else {
                    DeletionOutcome::Confirmed
                };
                (entry.target, outcome)
            })
            .collect();
        let proof = Self {
            schema_version: super::ERASURE_CONTRACT_SCHEMA_VERSION,
            scope_commitment: scope_commitment(plan),
            source_count: plan.scope.source_ids.len() as u32,
            target_outcomes: outcomes,
            redaction: ProofRedaction::ContentAndIdentifiersOmitted,
        };
        proof.validate()?;
        Ok(proof)
    }

    pub fn verify_for(
        &self,
        plan: &DeletionPlan,
        reconciliation: &ReconciliationReceipt,
    ) -> ErasureResult<()> {
        let expected = Self::from_reconciliation(plan, reconciliation)?;
        if self != &expected {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ProofInvalid,
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> ErasureResult<()> {
        if self.schema_version != super::ERASURE_CONTRACT_SCHEMA_VERSION
            || self.redaction != ProofRedaction::ContentAndIdentifiersOmitted
            || self.target_outcomes.len() != DeletionTarget::ALL.len()
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ProofInvalid,
            ));
        }
        let targets: BTreeSet<_> = self.target_outcomes.keys().copied().collect();
        if !DeletionTarget::ALL
            .iter()
            .all(|target| targets.contains(target))
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ProofInvalid,
            ));
        }
        Ok(())
    }
}

fn scope_commitment(plan: &DeletionPlan) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"verbatim-erasure-proof-v1\0");
    for source_id in &plan.scope.source_ids {
        hasher.update((source_id.len() as u64).to_be_bytes());
        hasher.update(source_id.as_bytes());
    }
    hasher.finalize().into()
}
