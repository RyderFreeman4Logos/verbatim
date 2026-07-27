//! Pure `plan → propagate → reconcile → report` adapter boundary.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    DeletionPolicy, DeletionProof, DeletionPropagationMatrix, DeletionScope, DeletionTarget,
    ErasureDiagnosticCode, ErasureError, ErasureResult, RemoteReconciliation,
};

/// Serializable, validated input to future backend adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionPlan {
    pub schema_version: u32,
    pub scope: DeletionScope,
    pub policy: DeletionPolicy,
    pub matrix: DeletionPropagationMatrix,
}

impl DeletionPlan {
    pub fn new(
        scope: DeletionScope,
        policy: DeletionPolicy,
        matrix: DeletionPropagationMatrix,
    ) -> ErasureResult<Self> {
        let plan = Self {
            schema_version: super::ERASURE_CONTRACT_SCHEMA_VERSION,
            scope,
            policy,
            matrix,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> ErasureResult<()> {
        if self.schema_version != super::ERASURE_CONTRACT_SCHEMA_VERSION {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::InvalidPlanJson,
            ));
        }
        self.matrix.validate()?;
        self.scope.validate(&self.matrix)?;
        self.policy.validate()
    }
}

/// Ordered propagation receipt. A future adapter must construct this only after
/// each local backend acknowledges its recorded matrix entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropagationReceipt {
    pub execution_order: Vec<DeletionTarget>,
    pub completed_targets: BTreeSet<DeletionTarget>,
    pub pending_remote_targets: BTreeSet<DeletionTarget>,
}

impl PropagationReceipt {
    pub fn complete(plan: &DeletionPlan) -> ErasureResult<Self> {
        plan.validate()?;
        let receipt = Self {
            execution_order: plan.matrix.ordered_targets(),
            completed_targets: plan.matrix.ordered_targets().into_iter().collect(),
            pending_remote_targets: BTreeSet::new(),
        };
        receipt.validate(plan)?;
        Ok(receipt)
    }

    pub fn with_pending_remote(
        plan: &DeletionPlan,
        pending_remote_targets: BTreeSet<DeletionTarget>,
    ) -> ErasureResult<Self> {
        plan.validate()?;
        let completed_targets = plan
            .matrix
            .ordered_targets()
            .into_iter()
            .filter(|target| !pending_remote_targets.contains(target))
            .collect();
        let receipt = Self {
            execution_order: plan.matrix.ordered_targets(),
            completed_targets,
            pending_remote_targets,
        };
        receipt.validate(plan)?;
        Ok(receipt)
    }

    pub fn validate(&self, plan: &DeletionPlan) -> ErasureResult<()> {
        if self.execution_order != plan.matrix.ordered_targets() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::PropagationOrderInvalid,
            ));
        }
        if self
            .pending_remote_targets
            .iter()
            .any(|target| !target.is_remote_replica())
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RemoteTargetRequired,
            ));
        }
        if !self
            .completed_targets
            .is_disjoint(&self.pending_remote_targets)
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::PropagationCoverageInvalid,
            ));
        }
        let all_targets: BTreeSet<_> = self
            .completed_targets
            .union(&self.pending_remote_targets)
            .copied()
            .collect();
        let expected_targets: BTreeSet<_> = plan.matrix.ordered_targets().into_iter().collect();
        if all_targets != expected_targets {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::PropagationCoverageInvalid,
            ));
        }
        Ok(())
    }
}

/// Couples ordered propagation with mandatory remote dead-letter handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationReceipt {
    pub propagation: PropagationReceipt,
    pub remote: RemoteReconciliation,
}

impl ReconciliationReceipt {
    pub fn new(
        propagation: PropagationReceipt,
        remote: RemoteReconciliation,
    ) -> ErasureResult<Self> {
        let receipt = Self {
            propagation,
            remote,
        };
        receipt.remote.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self, plan: &DeletionPlan) -> ErasureResult<()> {
        self.propagation.validate(plan)?;
        self.remote.validate()?;
        if self.propagation.pending_remote_targets != self.remote.remote_failures {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ReconciliationMismatch,
            ));
        }
        Ok(())
    }
}

/// Adapter boundary for a future live backend implementation. Implementations
/// cannot omit a lifecycle phase because the trait exposes only this sequence.
pub trait DeletionWorkflow {
    fn plan(&self, scope: DeletionScope, policy: DeletionPolicy) -> ErasureResult<DeletionPlan>;
    fn propagate(&self, plan: &DeletionPlan) -> ErasureResult<PropagationReceipt>;
    fn reconcile(&self, propagation: PropagationReceipt) -> ErasureResult<ReconciliationReceipt>;
    fn report(
        &self,
        plan: &DeletionPlan,
        reconciliation: &ReconciliationReceipt,
    ) -> ErasureResult<DeletionProof>;
}

pub fn encode_deletion_plan_json(plan: &DeletionPlan) -> ErasureResult<Vec<u8>> {
    plan.validate()?;
    serde_json::to_vec(plan)
        .map_err(|_| ErasureError::validation(ErasureDiagnosticCode::PlanSerializationFailed))
}

/// Decodes untrusted plan JSON then validates its complete inventory, ordering,
/// stale-read fence, policy propagation, and legal-hold condition.
pub fn decode_deletion_plan_json(bytes: &[u8]) -> ErasureResult<DeletionPlan> {
    let plan: DeletionPlan = serde_json::from_slice(bytes)
        .map_err(|_| ErasureError::validation(ErasureDiagnosticCode::InvalidPlanJson))?;
    plan.validate()?;
    Ok(plan)
}
