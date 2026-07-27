use super::*;

fn candidate(
    hit_id: &str,
    raw_rank: u32,
    occurrences: u64,
    evidence_strength: EvidenceStrength,
) -> RawCandidate {
    RawCandidate::new(RawCandidateFields {
        hit_id: hit_id.into(),
        raw_rank: RawRank::new(raw_rank).expect("test raw rank"),
        occurrence_count: OccurrenceCount::new(occurrences).expect("test occurrence count"),
        evidence_strength,
        semantic_distinction: SemanticDistinction::Equivalent,
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

#[test]
fn every_group_identity_variant_is_accepted_with_a_provenance_key() {
    let raw =
        RawCandidateRanking::new(vec![candidate("candidate", 1, 1, EvidenceStrength::Direct)])
            .expect("test ranking");
    let cases = vec![
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
    ];

    for (identity, collapse_reason) in cases {
        let group = DiversityGroup::new(
            DiversityGroupFields {
                identity,
                representative_hit_id: "candidate".into(),
                member_hit_ids: vec!["candidate".into()],
                collapse_reason,
            },
            &raw,
        );
        assert!(group.is_ok());
    }
}

#[test]
fn direct_evidence_and_distinct_translations_fail_closed() {
    let raw = RawCandidateRanking::new(vec![
        candidate("direct", 1, 1, EvidenceStrength::Direct),
        candidate("thematic", 2, 1, EvidenceStrength::Thematic),
        RawCandidate::new(RawCandidateFields {
            hit_id: "translation".into(),
            raw_rank: RawRank::new(3).expect("test raw rank"),
            occurrence_count: OccurrenceCount::new(1).expect("test occurrence count"),
            evidence_strength: EvidenceStrength::Direct,
            semantic_distinction: SemanticDistinction::SemanticallyDistinctTranslation,
        })
        .expect("test translation"),
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

    let similarity_only_translation = DiversityGroup::new(
        DiversityGroupFields {
            identity: GroupIdentity::NearDuplicate {
                similarity_basis: "embedding".into(),
            },
            representative_hit_id: "direct".into(),
            member_hit_ids: vec!["direct".into(), "translation".into()],
            collapse_reason: CollapseReason::NearDuplicate,
        },
        &raw,
    );
    assert!(matches!(
        similarity_only_translation,
        Err(DiversityError::Validation { .. })
    ));
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
