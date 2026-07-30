//! Validated search request, predicate, authorization, deadline, and work budget.

use crate::search_planner::SearchBudget;

use super::{
    DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult, IdempotencyKey,
    RequestIdentity,
};

const MAX_FILTER_VALUES: usize = 64;
const MAX_FILTER_VALUE_LEN: usize = 256;

/// Predicate plan used only for metadata exclusion before vector I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicatePlan {
    tenant: String,
    sources: Vec<String>,
    collections: Vec<String>,
    required_acls: Vec<String>,
}

impl PredicatePlan {
    pub fn new(
        tenant: impl Into<String>,
        sources: Vec<String>,
        collections: Vec<String>,
        required_acls: Vec<String>,
    ) -> DiskAnn3ServiceResult<Self> {
        let plan = Self {
            tenant: tenant.into(),
            sources,
            collections,
            required_acls,
        };
        if !valid_value(&plan.tenant)
            || plan.sources.is_empty()
            || plan.collections.is_empty()
            || plan.required_acls.is_empty()
            || !valid_values(&plan.sources)
            || !valid_values(&plan.collections)
            || !valid_values(&plan.required_acls)
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidPredicate,
            ));
        }
        Ok(plan)
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn sources(&self) -> &[String] {
        &self.sources
    }
    pub fn collections(&self) -> &[String] {
        &self.collections
    }
    pub fn required_acls(&self) -> &[String] {
        &self.required_acls
    }
}

/// Authorization is either cryptographically/authoritatively attested or uncertain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationContext {
    Attested {
        tenant: String,
        allowed_acls: Vec<String>,
    },
    Uncertain,
}

impl AuthorizationContext {
    pub fn attested(
        tenant: impl Into<String>,
        allowed_acls: Vec<String>,
    ) -> DiskAnn3ServiceResult<Self> {
        let tenant = tenant.into();
        if !valid_value(&tenant) || allowed_acls.is_empty() || !valid_values(&allowed_acls) {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidAuthorization,
            ));
        }
        Ok(Self::Attested {
            tenant,
            allowed_acls,
        })
    }

    pub const fn uncertain() -> Self {
        Self::Uncertain
    }

    pub fn authorizes(&self, predicate: &PredicatePlan) -> DiskAnn3ServiceResult<()> {
        let Self::Attested {
            tenant,
            allowed_acls,
        } = self
        else {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::AuthorizationUncertain,
            ));
        };
        if tenant != predicate.tenant()
            || !predicate
                .required_acls()
                .iter()
                .all(|acl| allowed_acls.contains(acl))
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidAuthorization,
            ));
        }
        Ok(())
    }
}

/// Bounded trace correlation token carried across local and remote semantic paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext(String);

impl TraceContext {
    pub fn new(value: impl Into<String>) -> DiskAnn3ServiceResult<Self> {
        let value = value.into();
        if !valid_value(&value) {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidRequest,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fully validated request with a bounded exact-dimension query vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    identity: RequestIdentity,
    predicate: PredicatePlan,
    authorization: AuthorizationContext,
    trace: TraceContext,
    budget: SearchBudget,
    deadline_micros: u64,
    query_vector: Vec<f32>,
    idempotency_key: IdempotencyKey,
}

impl SearchRequest {
    pub const DIMENSION: usize = 4_096;

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: RequestIdentity,
        predicate: PredicatePlan,
        authorization: AuthorizationContext,
        trace: TraceContext,
        budget: SearchBudget,
        deadline_micros: u64,
        query_vector: Vec<f32>,
        idempotency_key: IdempotencyKey,
    ) -> DiskAnn3ServiceResult<Self> {
        budget.validate().map_err(|_| {
            DiskAnn3ServiceError::contract(DiskAnn3ServiceDiagnosticCode::BudgetExceeded)
        })?;
        if deadline_micros == 0 || deadline_micros > budget.fields().max_wall_time_micros {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::DeadlineExceeded,
            ));
        }
        if query_vector.len() != Self::DIMENSION
            || query_vector.iter().any(|value| !value.is_finite())
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidRequest,
            ));
        }
        Ok(Self {
            identity,
            predicate,
            authorization,
            trace,
            budget,
            deadline_micros,
            query_vector,
            idempotency_key,
        })
    }

    pub fn with_authorization(mut self, authorization: AuthorizationContext) -> Self {
        self.authorization = authorization;
        self
    }

    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn predicate(&self) -> &PredicatePlan {
        &self.predicate
    }
    pub const fn authorization(&self) -> &AuthorizationContext {
        &self.authorization
    }
    pub const fn trace(&self) -> &TraceContext {
        &self.trace
    }
    pub const fn budget(&self) -> SearchBudget {
        self.budget
    }
    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }
    pub fn query_vector(&self) -> &[f32] {
        &self.query_vector
    }
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

fn valid_values(values: &[String]) -> bool {
    values.len() <= MAX_FILTER_VALUES && values.iter().all(|value| valid_value(value))
}

fn valid_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_FILTER_VALUE_LEN
}
