//! LanceDB-primary, generation-bound search policy and validated request envelope.

use crate::search_planner::SearchBudget;

use super::{
    AdaptiveProbePlan, LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult,
    LanceDbCollectionIdentity, LanceDbCollectionSchema, LanceDbFilterContract,
    LanceDbOperationBudget, LanceDbQualityPlan,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSelection {
    LanceDbPrimary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanceDbSearchPolicy {
    selection: BackendSelection,
    identity: LanceDbCollectionIdentity,
    caller_budget: SearchBudget,
}

impl LanceDbSearchPolicy {
    pub fn lancedb_primary(
        identity: LanceDbCollectionIdentity,
        caller_budget: SearchBudget,
    ) -> LanceDbBackendResult<Self> {
        caller_budget.validate().map_err(|_| {
            LanceDbBackendError::contract(LanceDbBackendDiagnosticCode::InvalidSearchPolicy)
        })?;
        Ok(Self {
            selection: BackendSelection::LanceDbPrimary,
            identity,
            caller_budget,
        })
    }

    pub const fn selection(&self) -> BackendSelection {
        self.selection
    }

    pub const fn identity(&self) -> &LanceDbCollectionIdentity {
        &self.identity
    }

    pub const fn caller_budget(&self) -> SearchBudget {
        self.caller_budget
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanceDbSearchRequest {
    schema: LanceDbCollectionSchema,
    policy: LanceDbSearchPolicy,
    filter: LanceDbFilterContract,
    probes: AdaptiveProbePlan,
    quality: LanceDbQualityPlan,
    budget: LanceDbOperationBudget,
    limit: u32,
}

impl LanceDbSearchRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: LanceDbCollectionSchema,
        policy: LanceDbSearchPolicy,
        filter: LanceDbFilterContract,
        probes: AdaptiveProbePlan,
        quality: LanceDbQualityPlan,
        budget: LanceDbOperationBudget,
        limit: u32,
    ) -> LanceDbBackendResult<Self> {
        if schema.identity() != policy.identity()
            || schema.identity().generation() != policy.identity().generation()
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::GenerationMismatch,
            ));
        }
        if limit == 0 || limit > budget.operation_budget().fields().result_limit {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidSearchPolicy,
            ));
        }
        Ok(Self {
            schema,
            policy,
            filter,
            probes,
            quality,
            budget,
            limit,
        })
    }

    pub const fn schema(&self) -> &LanceDbCollectionSchema {
        &self.schema
    }

    pub const fn policy(&self) -> &LanceDbSearchPolicy {
        &self.policy
    }

    pub const fn filter(&self) -> &LanceDbFilterContract {
        &self.filter
    }

    pub const fn probes(&self) -> &AdaptiveProbePlan {
        &self.probes
    }

    pub const fn quality(&self) -> &LanceDbQualityPlan {
        &self.quality
    }

    pub const fn budget(&self) -> &LanceDbOperationBudget {
        &self.budget
    }

    pub const fn limit(&self) -> u32 {
        self.limit
    }
}
