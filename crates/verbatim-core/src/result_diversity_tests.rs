use super::*;

fn candidate(
    hit_id: &str,
    raw_rank: u32,
    occurrences: u64,
    evidence_strength: EvidenceStrength,
) -> RawCandidate {
    candidate_with_distinction(
        hit_id,
        raw_rank,
        occurrences,
        evidence_strength,
        SemanticDistinction::Equivalent,
    )
}

fn candidate_with_distinction(
    hit_id: &str,
    raw_rank: u32,
    occurrences: u64,
    evidence_strength: EvidenceStrength,
    semantic_distinction: SemanticDistinction,
) -> RawCandidate {
    RawCandidate::new(RawCandidateFields {
        hit_id: hit_id.into(),
        raw_rank: RawRank::new(raw_rank).expect("test raw rank"),
        occurrence_count: OccurrenceCount::new(occurrences).expect("test occurrence count"),
        evidence_strength,
        semantic_distinction,
    })
    .expect("test candidate")
}

fn profile() -> DiversityProfile {
    DiversityProfile::new(DiversityProfileFields {
        version: 1,
        near_duplicate_threshold_basis_points: 9_000,
        max_per_source: Some(2),
        max_per_thread: Some(2),
        enable_mmr: false,
    })
    .expect("test profile")
}

#[test]
fn collapse_keeps_raw_ranking_occurrences_and_collapsed_member_discoverable() {
    let raw = RawCandidateRanking::new(vec![
        candidate("direct", 1, 3, EvidenceStrength::Direct),
        candidate("mirror", 2, 7, EvidenceStrength::Thematic),
    ])
    .expect("test ranking");
    let group = DiversityGroup::new(
        DiversityGroupFields {
            identity: GroupIdentity::ExactDuplicate {
                content_hash: "content-sha256".into(),
            },
            representative_hit_id: "direct".into(),
            member_hit_ids: vec!["direct".into(), "mirror".into()],
            collapse_reason: CollapseReason::ExactDuplicate,
        },
        &raw,
    )
    .expect("test group");

    let output = DiversityStageOutput::new(
        profile(),
        raw.clone(),
        vec![group],
        &DiversityBudget::skeleton_default(),
    )
    .expect("test output");

    let encoded = encode_diversity_stage_output_json(&output).expect("output encodes");
    let decoded = decode_diversity_stage_output_json(&encoded).expect("output decodes");
    assert_eq!(decoded, output);

    assert_eq!(
        raw.candidate("mirror")
            .expect("mirror exists")
            .occurrence_count()
            .get(),
        7
    );
    assert_eq!(
        output
            .raw_ranking()
            .candidate("mirror")
            .expect("raw mirror remains")
            .raw_rank()
            .get(),
        2
    );
    let collapsed = output
        .collapsed_member("mirror")
        .expect("collapsed member remains visible");
    assert_eq!(collapsed.raw_rank().get(), 2);
    assert_eq!(
        collapsed.collapse_reason(),
        Some(&CollapseReason::ExactDuplicate)
    );
}

fn identity_reason_cases() -> Vec<(GroupIdentity, CollapseReason)> {
    vec![
        (
            GroupIdentity::ExactDuplicate {
                content_hash: "hash".into(),
            },
            CollapseReason::ExactDuplicate,
        ),
        (
            GroupIdentity::NearDuplicate {
                similarity_basis: "normalized-shingles".into(),
            },
            CollapseReason::NearDuplicate,
        ),
        (
            GroupIdentity::Overlap {
                normalized_span_hash: "span-hash".into(),
            },
            CollapseReason::Overlap,
        ),
        (
            GroupIdentity::ParentChild {
                parent_hit_id: "parent".into(),
            },
            CollapseReason::ParentChild,
        ),
        (
            GroupIdentity::Thread {
                thread_id: "thread".into(),
            },
            CollapseReason::ThreadQuota,
        ),
        (
            GroupIdentity::Source {
                source_id: "source".into(),
            },
            CollapseReason::SourceQuota,
        ),
        (
            GroupIdentity::Mirror {
                mirror_family_id: "mirror".into(),
            },
            CollapseReason::Mirror,
        ),
        (
            GroupIdentity::Version {
                version_family_id: "version".into(),
            },
            CollapseReason::ExplicitEquivalentVersion,
        ),
    ]
}

fn all_collapse_reasons() -> [CollapseReason; 8] {
    [
        CollapseReason::ExactDuplicate,
        CollapseReason::NearDuplicate,
        CollapseReason::Overlap,
        CollapseReason::ParentChild,
        CollapseReason::ThreadQuota,
        CollapseReason::SourceQuota,
        CollapseReason::Mirror,
        CollapseReason::ExplicitEquivalentVersion,
    ]
}

#[test]
fn every_group_identity_binds_to_its_reason_and_attributes_multi_member_collapse() {
    let raw = RawCandidateRanking::new(vec![
        candidate("representative", 1, 1, EvidenceStrength::Direct),
        candidate("collapsed", 2, 1, EvidenceStrength::Thematic),
    ])
    .expect("test ranking");

    for (identity, compatible_reason) in identity_reason_cases() {
        for collapse_reason in all_collapse_reasons() {
            let group = DiversityGroup::new(
                DiversityGroupFields {
                    identity: identity.clone(),
                    representative_hit_id: "representative".into(),
                    member_hit_ids: vec!["representative".into(), "collapsed".into()],
                    collapse_reason: collapse_reason.clone(),
                },
                &raw,
            );
            assert_eq!(
                group.is_ok(),
                collapse_reason == compatible_reason,
                "identity/reason compatibility must be exhaustive"
            );
            if let Ok(group) = group {
                assert_eq!(
                    group
                        .members()
                        .iter()
                        .find(|member| member.hit_id() == "collapsed")
                        .and_then(GroupedMember::collapse_reason),
                    Some(&collapse_reason)
                );
            }
        }
    }
}

#[test]
fn direct_evidence_and_distinct_semantics_fail_closed() {
    let raw = RawCandidateRanking::new(vec![
        candidate("direct", 1, 1, EvidenceStrength::Direct),
        candidate("thematic", 2, 1, EvidenceStrength::Thematic),
        candidate_with_distinction(
            "translation",
            3,
            1,
            EvidenceStrength::Direct,
            SemanticDistinction::SemanticallyDistinctTranslation,
        ),
    ])
    .expect("test ranking");

    let weaker_representative = DiversityGroup::new(
        DiversityGroupFields {
            identity: GroupIdentity::Source {
                source_id: "source".into(),
            },
            representative_hit_id: "thematic".into(),
            member_hit_ids: vec!["direct".into(), "thematic".into()],
            collapse_reason: CollapseReason::SourceQuota,
        },
        &raw,
    );
    assert!(matches!(
        weaker_representative,
        Err(DiversityError::Validation { .. })
    ));

    for distinction in [
        SemanticDistinction::LegallyDistinctVersion,
        SemanticDistinction::SemanticallyDistinctTranslation,
    ] {
        let protected_raw = RawCandidateRanking::new(vec![
            candidate("representative", 1, 1, EvidenceStrength::Direct),
            candidate_with_distinction("protected", 2, 1, EvidenceStrength::Thematic, distinction),
        ])
        .expect("test protected ranking");
        for (identity, collapse_reason) in [
            (
                GroupIdentity::NearDuplicate {
                    similarity_basis: "embedding".into(),
                },
                CollapseReason::Overlap,
            ),
            (
                GroupIdentity::Overlap {
                    normalized_span_hash: "span".into(),
                },
                CollapseReason::NearDuplicate,
            ),
        ] {
            assert!(matches!(
                DiversityGroup::new(
                    DiversityGroupFields {
                        identity,
                        representative_hit_id: "representative".into(),
                        member_hit_ids: vec!["representative".into(), "protected".into()],
                        collapse_reason,
                    },
                    &protected_raw,
                ),
                Err(DiversityError::Validation { .. })
            ));
        }
    }
}

#[test]
fn profile_budget_and_mode_typed_stage_machine_are_checked() {
    let first = profile();
    let second = profile();
    assert_eq!(first.version(), 1);
    assert_eq!(first.profile_hash(), second.profile_hash());
    assert!(DiversityProfile::new(DiversityProfileFields {
        version: 0,
        near_duplicate_threshold_basis_points: 9_000,
        max_per_source: None,
        max_per_thread: None,
        enable_mmr: true,
    })
    .is_err());

    let over_budget = DiversityUsage {
        raw_candidates: 2,
        groups: 1,
        collapsed_members: 1,
    }
    .check(
        &DiversityBudget::new(DiversityBudgetFields {
            max_raw_candidates: 1,
            max_groups: 1,
            max_collapsed_members: 1,
        })
        .expect("test budget"),
    );
    assert!(matches!(
        over_budget,
        Err(DiversityError::BudgetExhausted { .. })
    ));

    let mut run = DiversityRun::<ContextPack>::new();
    run.advance(DiversityStage::SelectingRepresentatives)
        .expect("legal stage transition");
    assert!(matches!(
        run.advance(DiversityStage::Complete),
        Err(DiversityError::IllegalTransition { .. })
    ));
}

#[test]
fn every_mode_instantiates_a_request_and_run() {
    fn assert_mode<M: DiversityMode>() {
        let raw =
            RawCandidateRanking::new(vec![candidate("candidate", 1, 1, EvidenceStrength::Direct)])
                .expect("test ranking");
        let request =
            DiversityRequest::<M>::new(raw, profile(), DiversityBudget::skeleton_default());
        assert_eq!(request.raw_ranking().candidates().len(), 1);
        assert_eq!(
            DiversityRun::<M>::new().current_stage(),
            DiversityStage::Grouping
        );
    }

    assert_mode::<ExploratorySearch>();
    assert_mode::<PrecisionRetrieve>();
    assert_mode::<ContextPack>();
    assert_mode::<Exhaustive>();
}

#[test]
fn diversity_errors_do_not_render_secret_bearing_input() {
    let secret = "credential=top-secret";
    let error = DiversityError::validation(DiversityDiagnosticCode::RawRankMustBePositive);

    assert_eq!(
        format!("{error:?}"),
        "DiversityError(raw_rank_must_be_positive)"
    );
    assert_eq!(
        error.to_string(),
        "result-diversity.raw_rank_must_be_positive"
    );
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}
