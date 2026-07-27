use super::*;

fn candidates(generation: PublicationGeneration) -> Vec<VectorCandidate> {
    vec![
        VectorCandidate::new("a", 0.5, generation, false).expect("candidate"),
        VectorCandidate::new("b", 0.5, generation, false).expect("candidate"),
        VectorCandidate::new("c", 0.5, generation, false).expect("candidate"),
    ]
}

fn stage_budget(caps: [u32; 6]) -> RetrievalStageBudget {
    RetrievalStageBudget::new(caps).expect("stage caps")
}

#[test]
fn each_retrieval_stage_enforces_its_own_output_cap() {
    let generation = PublicationGeneration::new(1).expect("generation");

    assert_eq!(
        BoundedCandidates::new(
            candidates(generation),
            generation,
            &stage_budget([2, 3, 3, 3, 3, 3])
        )
        .expect_err("candidate generation cap")
        .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );

    let generated = BoundedCandidates::new(
        candidates(generation),
        generation,
        &stage_budget([3, 2, 3, 3, 3, 3]),
    )
    .expect("generated candidates");
    assert_eq!(
        generated
            .rescore(&stage_budget([3, 2, 3, 3, 3, 3]))
            .expect_err("rescore cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );

    let rescored = BoundedCandidates::new(
        candidates(generation),
        generation,
        &stage_budget([3, 3, 2, 3, 3, 3]),
    )
    .expect("generated candidates")
    .rescore(&stage_budget([3, 3, 2, 3, 3, 3]))
    .expect("rescored candidates");
    assert_eq!(
        rescored
            .apply_filters(&stage_budget([3, 3, 2, 3, 3, 3]))
            .expect_err("filter-application cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );

    let filtered = BoundedCandidates::new(
        candidates(generation),
        generation,
        &stage_budget([3, 3, 3, 2, 3, 3]),
    )
    .expect("generated candidates")
    .rescore(&stage_budget([3, 3, 3, 2, 3, 3]))
    .expect("rescored candidates")
    .apply_filters(&stage_budget([3, 3, 3, 2, 3, 3]))
    .expect("filtered candidates");
    let fused = filtered
        .fuse(&stage_budget([3, 3, 3, 2, 3, 3]))
        .expect("fusion truncates to its cap");
    assert_eq!(fused.candidates().len(), 2);

    let fused = BoundedCandidates::new(
        candidates(generation),
        generation,
        &stage_budget([3, 3, 3, 3, 2, 3]),
    )
    .expect("generated candidates")
    .rescore(&stage_budget([3, 3, 3, 3, 2, 3]))
    .expect("rescored candidates")
    .apply_filters(&stage_budget([3, 3, 3, 3, 2, 3]))
    .expect("filtered candidates")
    .fuse(&stage_budget([3, 3, 3, 3, 2, 3]))
    .expect("fused candidates");
    assert_eq!(
        fused
            .rerank(&stage_budget([3, 3, 3, 3, 2, 3]))
            .expect_err("rerank cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );

    let reranked = BoundedCandidates::new(
        candidates(generation),
        generation,
        &stage_budget([3, 3, 3, 3, 3, 2]),
    )
    .expect("generated candidates")
    .rescore(&stage_budget([3, 3, 3, 3, 3, 2]))
    .expect("rescored candidates")
    .apply_filters(&stage_budget([3, 3, 3, 3, 3, 2]))
    .expect("filtered candidates")
    .fuse(&stage_budget([3, 3, 3, 3, 3, 2]))
    .expect("fused candidates")
    .rerank(&stage_budget([3, 3, 3, 3, 3, 2]))
    .expect("reranked candidates");
    assert_eq!(
        reranked
            .hydrate(&stage_budget([3, 3, 3, 3, 3, 2]))
            .expect_err("hydration cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );
}
