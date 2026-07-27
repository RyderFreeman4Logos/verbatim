//! Retry, dead-letter, and operator-alert contract for remote replicas.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{DeletionTarget, ErasureDiagnosticCode, ErasureError, ErasureResult};

/// Bounded retry policy for remote deletion work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub max_delay_seconds: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_delay_seconds: 300,
        }
    }
}

impl RetryPolicy {
    pub fn validate(self) -> ErasureResult<()> {
        if self.max_attempts == 0 {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RetryAttemptsMustBePositive,
            ));
        }
        if self.max_delay_seconds == 0 {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RetryDelayMustBePositive,
            ));
        }
        Ok(())
    }
}

/// Persisted handling state for a failed remote deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadLetterState {
    NotRequired,
    Enqueued,
    Resolved,
}

/// Required operator notification state. A contract adapter must issue an alert
/// when work is dead-lettered; the alert contains no restricted source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorAlertState {
    NotRequired,
    Required,
    Acknowledged,
}

/// Reconciliation record for remote/backlogged failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteReconciliation {
    pub retry: RetryPolicy,
    pub remote_failures: BTreeSet<DeletionTarget>,
    pub dead_letter: DeadLetterState,
    pub operator_alert: OperatorAlertState,
}

impl RemoteReconciliation {
    pub fn complete(retry: RetryPolicy) -> Self {
        Self {
            retry,
            remote_failures: BTreeSet::new(),
            dead_letter: DeadLetterState::NotRequired,
            operator_alert: OperatorAlertState::NotRequired,
        }
    }

    pub fn remote_failure(target: DeletionTarget, retry: RetryPolicy) -> ErasureResult<Self> {
        if !target.is_remote_replica() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RemoteTargetRequired,
            ));
        }
        let reconciliation = Self {
            retry,
            remote_failures: [target].into_iter().collect(),
            dead_letter: DeadLetterState::Enqueued,
            operator_alert: OperatorAlertState::Required,
        };
        reconciliation.validate()?;
        Ok(reconciliation)
    }

    pub fn validate(&self) -> ErasureResult<()> {
        self.retry.validate()?;
        if self
            .remote_failures
            .iter()
            .any(|target| !target.is_remote_replica())
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RemoteTargetRequired,
            ));
        }
        if self.remote_failures.is_empty() {
            return Ok(());
        }
        if !matches!(
            self.dead_letter,
            DeadLetterState::Enqueued | DeadLetterState::Resolved
        ) {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RemoteFailureDeadLetterRequired,
            ));
        }
        if !matches!(
            self.operator_alert,
            OperatorAlertState::Required | OperatorAlertState::Acknowledged
        ) {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::RemoteFailureOperatorAlertRequired,
            ));
        }
        Ok(())
    }
}
