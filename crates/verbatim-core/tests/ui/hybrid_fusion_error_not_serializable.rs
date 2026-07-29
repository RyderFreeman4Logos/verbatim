use verbatim_core::hybrid_fusion::{
    CompletenessState, FusionDiagnosticCode, FusionError,
};

fn main() {
    let error = FusionError::completeness_violation(
        CompletenessState::ApproximateTopK,
        FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
    );
    let _json = serde_json::to_string(&error);
}
