//! Result-diversity / near-duplicate-collapse workflow contract (issue #361).
//!
//! This pure walking skeleton preserves an immutable raw ranking and exhaustive
//! occurrence counts while representing presentation/context collapse as an
//! inspectable projection. It contains no live retrieval, embedding/MMR
//! implementation, Store, SQL, filesystem, daemon, CLI, or UI wiring.
//! See `docs/architecture/result-diversity-workflow.md`.

mod budget;
mod error;
mod model;
mod output;
mod profile;
mod stage;
mod workflow;

pub use budget::{
    DiversityBudget, DiversityBudgetDimension, DiversityBudgetExhaustion, DiversityBudgetFields,
    DiversityUsage,
};
pub use error::{DiversityDiagnosticCode, DiversityError, DiversityResult};
pub use model::{
    CollapseReason, DiversityGroup, DiversityGroupFields, EvidenceStrength, GroupIdentity,
    GroupedMember, OccurrenceCount, RawCandidate, RawCandidateFields, RawCandidateRanking, RawRank,
    SemanticDistinction,
};
pub use output::{
    decode_diversity_stage_output_json, encode_diversity_stage_output_json, DiversityStageOutput,
};
pub use profile::{DiversityProfile, DiversityProfileFields};
pub use stage::{DiversityRun, DiversityStage};
pub use workflow::{
    ContextPack, DiversityMode, DiversityRequest, Exhaustive, ExploratorySearch, PrecisionRetrieve,
    ResultDiversityWorkflow,
};

/// Contract schema version for result-diversity workflow documents.
pub const RESULT_DIVERSITY_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../result_diversity_tests.rs"]
mod tests;
