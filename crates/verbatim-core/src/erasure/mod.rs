//! Cross-backend deletion and verifiable-erasure contract (RIGHTS-002 / #363).
//!
//! This pure walking skeleton defines the complete inventory, retention and
//! revocation policy, fail-closed propagation order, remote reconciliation, and
//! redaction-safe proof boundary. It intentionally contains no live SQLite,
//! Tantivy, HNSW, Qdrant, graph, blob, cache, daemon, or CLI integration. See
//! `docs/architecture/erasure-workflow.md`.

mod error;
mod matrix;
mod policy;
mod product;
mod proof;
mod reconciliation;
mod scope;
mod state;
mod target;
mod workflow;

pub use error::{ErasureDiagnosticCode, ErasureError, ErasureResult};
pub use matrix::{DeletionMatrixEntry, DeletionOrdering, DeletionPropagationMatrix};
pub use policy::{CryptographicErasure, DeletionPolicy, KeyRotationRequirement, PolicyPropagation};
pub use product::DataProduct;
pub use proof::{DeletionOutcome, DeletionProof, ProofRedaction};
pub use reconciliation::{DeadLetterState, OperatorAlertState, RemoteReconciliation, RetryPolicy};
pub use scope::DeletionScope;
pub use state::DeletionState;
pub use target::DeletionTarget;
pub use workflow::{
    decode_deletion_plan_json, encode_deletion_plan_json, DeletionPlan, DeletionWorkflow,
    PropagationReceipt, ReconciliationReceipt,
};

/// Contract schema version for erasure plan and proof documents.
pub const ERASURE_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../erasure_tests.rs"]
mod tests;
