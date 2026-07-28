//! Focused unit tests for the AISAQ co-located SSD page-layout contract.
//!
//! Covers every validation path: page size/alignment, checksum torn-page
//! detection, strategy/rule compatibility and redundancy ceilings,
//! read-amplification bounds and exhaustion, spec cross-field invariants, and
//! the `SearchBudget` binding. No SSD I/O is performed.

#![cfg(test)]

use crate::page_layout::{
    PageAlignment, PageChecksum, PageLayoutDiagnosticCode, PageLayoutError, PageLayoutSpec,
    PageLayoutSpecFields, PageLayoutStrategy, PageSize, ReadAmplificationBound,
    COLOCATION_REDUNDANCY_CEILING_PERFORMANCE, COLOCATION_REDUNDANCY_CEILING_SCALE,
};
use crate::search_planner::{SearchBudget, SearchBudgetFields};

use super::checksum::ChecksumPolicy;
use super::colocation::ColocationRule;

// ----------------------------------------------------------------------------
// PageLayoutStrategy
// ----------------------------------------------------------------------------

#[test]
fn strategy_wire_names_are_stable() {
    assert_eq!(PageLayoutStrategy::VectorFirst.as_str(), "vector-first");
    assert_eq!(PageLayoutStrategy::GraphFirst.as_str(), "graph-first");
    assert_eq!(
        PageLayoutStrategy::ColocatedScale.as_str(),
        "colocated-scale"
    );
}

#[test]
fn strategy_colocation_flag_matches_issue_variants() {
    assert!(!PageLayoutStrategy::VectorFirst.is_colocated());
    assert!(PageLayoutStrategy::GraphFirst.is_colocated());
    assert!(PageLayoutStrategy::ColocatedScale.is_colocated());
}

#[test]
fn strategy_redundancy_ceilings_are_documented_and_ordered() {
    assert_eq!(
        PageLayoutStrategy::VectorFirst.redundancy_factor_ceiling(),
        1
    );
    assert_eq!(
        PageLayoutStrategy::GraphFirst.redundancy_factor_ceiling(),
        COLOCATION_REDUNDANCY_CEILING_PERFORMANCE
    );
    assert_eq!(
        PageLayoutStrategy::ColocatedScale.redundancy_factor_ceiling(),
        COLOCATION_REDUNDANCY_CEILING_SCALE
    );
    assert!(COLOCATION_REDUNDANCY_CEILING_SCALE < COLOCATION_REDUNDANCY_CEILING_PERFORMANCE);
}

#[test]
fn strategy_rejects_zero_redundancy_factor() {
    let err = PageLayoutStrategy::GraphFirst
        .validate_redundancy_factor(0)
        .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidRedundancyFactor
    );
}

#[test]
fn strategy_rejects_redundancy_above_ceiling() {
    let err = PageLayoutStrategy::ColocatedScale
        .validate_redundancy_factor(COLOCATION_REDUNDANCY_CEILING_SCALE + 1)
        .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::RedundancyFactorTooLarge
    );
}

#[test]
fn strategy_accepts_redundancy_at_ceiling() {
    PageLayoutStrategy::GraphFirst
        .validate_redundancy_factor(COLOCATION_REDUNDANCY_CEILING_PERFORMANCE)
        .unwrap();
}

// ----------------------------------------------------------------------------
// PageSize / PageAlignment
// ----------------------------------------------------------------------------

#[test]
fn canonical_page_sizes_are_power_of_two() {
    assert_eq!(PageSize::kib_4().bytes(), 4_096);
    assert_eq!(PageSize::kib_16().bytes(), 16_384);
    assert_eq!(PageSize::kib_64().bytes(), 65_536);
}

#[test]
fn page_size_rejects_zero() {
    let err = PageSize::new(0).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidPageSize
    );
}

#[test]
fn page_size_rejects_below_nvme_floor() {
    let err = PageSize::new(2_048).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::PageSizeTooSmall
    );
}

#[test]
fn page_size_rejects_above_ceiling() {
    let err = PageSize::new(1 << 21).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::PageSizeTooLarge
    );
}

#[test]
fn page_size_rejects_non_power_of_two() {
    let err = PageSize::new(5_000).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::PageSizeNotAligned
    );
}

#[test]
fn page_size_accepts_custom_aligned_within_range() {
    let page = PageSize::new(1 << 16).unwrap();
    assert_eq!(page.bytes(), 65_536);
}

#[test]
fn alignment_rejects_zero() {
    let err = PageAlignment::new(0, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidAlignment
    );
}

#[test]
fn alignment_rejects_non_power_of_two() {
    let err = PageAlignment::new(3_000, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidAlignment
    );
}

#[test]
fn alignment_rejects_non_divisor_of_page() {
    // 8192 does not divide 4096.
    let err = PageAlignment::new(8_192, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::AlignmentNotPageDivisor
    );
}

#[test]
fn alignment_accepts_page_divisor() {
    PageAlignment::new(4_096, PageSize::kib_64()).unwrap();
    PageAlignment::new(2_048, PageSize::kib_4()).unwrap();
}

#[test]
fn natural_alignment_equals_page_size() {
    assert_eq!(PageAlignment::natural(PageSize::kib_16()).bytes(), 16_384);
}

// ----------------------------------------------------------------------------
// PageChecksum / torn-page detection
// ----------------------------------------------------------------------------

#[test]
fn checksum_rejects_empty_payload() {
    let err = PageChecksum::from_payload(&[]).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::EmptyChecksumPayload
    );
}

#[test]
fn checksum_verifies_identical_payload() {
    let payload = b"graph-vertex-neighbor-codes";
    let checksum = PageChecksum::from_payload(payload).unwrap();
    checksum.verify(payload).unwrap();
}

#[test]
fn checksum_detects_torn_or_corrupted_page() {
    let original = b"graph-vertex-neighbor-codes";
    let torn = b"graph-vertex-neighbor-codex";
    let checksum = PageChecksum::from_payload(original).unwrap();
    let err = checksum.verify(torn).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ChecksumMismatch
    );
}

#[test]
fn checksum_stored_roundtrip_is_stable() {
    let payload = b"some page bytes";
    let checksum = PageChecksum::from_payload(payload).unwrap();
    let restored = PageChecksum::from_stored(checksum.bytes());
    assert_eq!(checksum, restored);
}

#[test]
fn checksum_policy_flag() {
    assert!(ChecksumPolicy::Enabled.is_enabled());
    assert!(!ChecksumPolicy::Disabled.is_enabled());
}

// ----------------------------------------------------------------------------
// ColocationRule
// ----------------------------------------------------------------------------

#[test]
fn colocation_redundancy_factor_accessor() {
    assert_eq!(
        ColocationRule::Separated {
            redundancy_factor: 1
        }
        .redundancy_factor(),
        1
    );
    assert_eq!(
        ColocationRule::FullColocation {
            redundancy_factor: 3
        }
        .redundancy_factor(),
        3
    );
}

#[test]
fn colocation_is_colocated_flag() {
    assert!(!ColocationRule::Separated {
        redundancy_factor: 1
    }
    .is_colocated());
    assert!(ColocationRule::FullColocation {
        redundancy_factor: 2
    }
    .is_colocated());
    assert!(ColocationRule::PartialColocation {
        redundancy_factor: 2
    }
    .is_colocated());
}

#[test]
fn colocation_rejects_strategy_mismatch() {
    // Separated is compatible only with VectorFirst.
    let err = ColocationRule::Separated {
        redundancy_factor: 1,
    }
    .validate(PageLayoutStrategy::GraphFirst)
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ColocationStrategyMismatch
    );

    // FullColocation is compatible only with GraphFirst.
    let err = ColocationRule::FullColocation {
        redundancy_factor: 2,
    }
    .validate(PageLayoutStrategy::VectorFirst)
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ColocationStrategyMismatch
    );

    // PartialColocation is compatible only with ColocatedScale.
    let err = ColocationRule::PartialColocation {
        redundancy_factor: 2,
    }
    .validate(PageLayoutStrategy::GraphFirst)
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ColocationStrategyMismatch
    );
}

#[test]
fn colocation_rejects_redundancy_above_strategy_ceiling() {
    let err = ColocationRule::FullColocation {
        redundancy_factor: COLOCATION_REDUNDANCY_CEILING_PERFORMANCE + 1,
    }
    .validate(PageLayoutStrategy::GraphFirst)
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::RedundancyFactorTooLarge
    );
}

#[test]
fn colocation_accepts_compatible_strategy_and_ceiling() {
    ColocationRule::Separated {
        redundancy_factor: 1,
    }
    .validate(PageLayoutStrategy::VectorFirst)
    .unwrap();
    ColocationRule::FullColocation {
        redundancy_factor: COLOCATION_REDUNDANCY_CEILING_PERFORMANCE,
    }
    .validate(PageLayoutStrategy::GraphFirst)
    .unwrap();
    ColocationRule::PartialColocation {
        redundancy_factor: COLOCATION_REDUNDANCY_CEILING_SCALE,
    }
    .validate(PageLayoutStrategy::ColocatedScale)
    .unwrap();
}

// ----------------------------------------------------------------------------
// ReadAmplificationBound
// ----------------------------------------------------------------------------

#[test]
fn read_amplification_rejects_zero_max_pages() {
    let err = ReadAmplificationBound::new(0, 8_192, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidMaxPages
    );
}

#[test]
fn read_amplification_rejects_zero_max_bytes() {
    let err = ReadAmplificationBound::new(4, 0, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::InvalidMaxBytes
    );
}

#[test]
fn read_amplification_rejects_bytes_below_one_page() {
    let err = ReadAmplificationBound::new(4, 4_095, PageSize::kib_4()).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::BytesBelowOnePage
    );
}

#[test]
fn read_amplification_admits_within_bounds() {
    let bound = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    assert!(bound.admits(5, 20_480));
    assert!(bound.admits(10, 40_960));
}

#[test]
fn read_amplification_rejects_over_bounds() {
    let bound = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    assert!(!bound.admits(11, 20_480));
    assert!(!bound.admits(5, 40_961));
}

#[test]
fn read_amplification_reports_pages_exhaustion() {
    let bound = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    assert_eq!(
        bound.exhaustion(11, 20_480),
        Some(super::ReadAmplificationExhaustion::MaxPages)
    );
}

#[test]
fn read_amplification_reports_bytes_exhaustion() {
    let bound = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    assert_eq!(
        bound.exhaustion(5, 40_961),
        Some(super::ReadAmplificationExhaustion::MaxBytes)
    );
}

#[test]
fn read_amplification_no_exhaustion_within_bounds() {
    let bound = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    assert_eq!(bound.exhaustion(10, 40_960), None);
}

#[test]
fn read_amplification_ensure_not_wider_than_rejects_widening() {
    let caller = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    let wider = ReadAmplificationBound::new(20, 40_960, PageSize::kib_4()).unwrap();
    let err = wider.ensure_not_wider_than(caller).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::BudgetExceeded
    );
}

#[test]
fn read_amplification_ensure_not_wider_than_accepts_narrower() {
    let caller = ReadAmplificationBound::new(10, 40_960, PageSize::kib_4()).unwrap();
    let narrower = ReadAmplificationBound::new(5, 20_480, PageSize::kib_4()).unwrap();
    narrower.ensure_not_wider_than(caller).unwrap();
}

// ----------------------------------------------------------------------------
// PageLayoutSpec cross-field invariants
// ----------------------------------------------------------------------------

fn valid_graph_first_spec() -> PageLayoutSpecFields {
    let page = PageSize::kib_4();
    PageLayoutSpecFields {
        strategy: PageLayoutStrategy::GraphFirst,
        page_size: page,
        alignment: PageAlignment::natural(page),
        checksum_policy: ChecksumPolicy::Enabled,
        colocation: ColocationRule::FullColocation {
            redundancy_factor: COLOCATION_REDUNDANCY_CEILING_PERFORMANCE,
        },
        read_amplification: ReadAmplificationBound::new(10, 40_960, page).unwrap(),
    }
}

#[test]
fn spec_constructs_valid_graph_first() {
    PageLayoutSpec::new(valid_graph_first_spec()).unwrap();
}

#[test]
fn spec_rejects_colocating_rule_without_checksums() {
    let mut fields = valid_graph_first_spec();
    fields.checksum_policy = ChecksumPolicy::Disabled;
    let err = PageLayoutSpec::new(fields).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ChecksumPolicyMismatch
    );
}

#[test]
fn spec_allows_separated_rule_without_checksums() {
    let page = PageSize::kib_4();
    let fields = PageLayoutSpecFields {
        strategy: PageLayoutStrategy::VectorFirst,
        page_size: page,
        alignment: PageAlignment::natural(page),
        checksum_policy: ChecksumPolicy::Disabled,
        colocation: ColocationRule::Separated {
            redundancy_factor: 1,
        },
        read_amplification: ReadAmplificationBound::new(10, 40_960, page).unwrap(),
    };
    PageLayoutSpec::new(fields).unwrap();
}

#[test]
fn spec_rejects_strategy_colocation_mismatch() {
    let mut fields = valid_graph_first_spec();
    fields.colocation = ColocationRule::Separated {
        redundancy_factor: 1,
    };
    let err = PageLayoutSpec::new(fields).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::ColocationStrategyMismatch
    );
}

#[test]
fn spec_rejects_redundancy_above_ceiling() {
    let mut fields = valid_graph_first_spec();
    fields.colocation = ColocationRule::FullColocation {
        redundancy_factor: COLOCATION_REDUNDANCY_CEILING_PERFORMANCE + 1,
    };
    let err = PageLayoutSpec::new(fields).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::RedundancyFactorTooLarge
    );
}

#[test]
fn spec_constructs_colocated_scale_variant() {
    let page = PageSize::kib_16();
    let fields = PageLayoutSpecFields {
        strategy: PageLayoutStrategy::ColocatedScale,
        page_size: page,
        alignment: PageAlignment::natural(page),
        checksum_policy: ChecksumPolicy::Enabled,
        colocation: ColocationRule::PartialColocation {
            redundancy_factor: COLOCATION_REDUNDANCY_CEILING_SCALE,
        },
        read_amplification: ReadAmplificationBound::new(8, 131_072, page).unwrap(),
    };
    let spec = PageLayoutSpec::new(fields).unwrap();
    assert_eq!(spec.strategy(), PageLayoutStrategy::ColocatedScale);
    assert_eq!(spec.page_size(), PageSize::kib_16());
}

// ----------------------------------------------------------------------------
// SearchBudget binding
// ----------------------------------------------------------------------------

fn budget_with_caps(max_ssd_pages: u64, max_bytes_read: u64) -> SearchBudget {
    SearchBudget::new(SearchBudgetFields {
        result_limit: 10,
        dense_candidate_limit: 100,
        lexical_candidate_limit: 100,
        exact_candidate_limit: 100,
        graph_candidate_limit: 100,
        fused_pool_limit: 200,
        rerank_candidate_limit: 150,
        full_precision_rescore_limit: 100,
        hydration_limit: 50,
        max_ssd_pages,
        max_bytes_read,
        max_cpu_micros: 1_000_000,
        max_work_units: 1_000_000,
        max_wall_time_micros: 1_000_000,
        max_concurrent_stages: 4,
        max_stage_attempts: 3,
        debug_record_limit: 10,
    })
    .expect("valid budget")
}

#[test]
fn spec_binds_to_budget_within_caps() {
    let spec = PageLayoutSpec::new(valid_graph_first_spec()).unwrap();
    let budget = budget_with_caps(100, 1_048_576);
    spec.bind_to_budget(&budget).unwrap();
}

#[test]
fn spec_rejects_page_bound_wider_than_budget() {
    let mut fields = valid_graph_first_spec();
    // widen read amplification beyond the budget's page cap.
    fields.read_amplification =
        ReadAmplificationBound::new(200, 40_960, PageSize::kib_4()).unwrap();
    let spec = PageLayoutSpec::new(fields).unwrap();
    let budget = budget_with_caps(100, 1_048_576);
    let err = spec.bind_to_budget(&budget).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::BudgetExceeded
    );
}

#[test]
fn spec_rejects_byte_bound_wider_than_budget() {
    let page = PageSize::kib_64();
    let fields = PageLayoutSpecFields {
        strategy: PageLayoutStrategy::GraphFirst,
        page_size: page,
        alignment: PageAlignment::natural(page),
        checksum_policy: ChecksumPolicy::Enabled,
        colocation: ColocationRule::FullColocation {
            redundancy_factor: COLOCATION_REDUNDANCY_CEILING_PERFORMANCE,
        },
        // byte bound wider than the budget's 1 MiB byte cap.
        read_amplification: ReadAmplificationBound::new(10, 2_097_152, page).unwrap(),
    };
    let spec = PageLayoutSpec::new(fields).unwrap();
    let budget = budget_with_caps(100, 1_048_576);
    let err = spec.bind_to_budget(&budget).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        PageLayoutDiagnosticCode::BudgetExceeded
    );
}

// ----------------------------------------------------------------------------
// Error redaction
// ----------------------------------------------------------------------------

#[test]
fn error_debug_renders_only_diagnostic_code() {
    let err = PageLayoutError::contract(PageLayoutDiagnosticCode::ChecksumMismatch);
    let debug = format!("{:?}", err);
    let display = format!("{}", err);
    assert_eq!(debug, "PageLayoutError(checksum_mismatch)");
    assert_eq!(display, "page-layout.checksum_mismatch");
}
