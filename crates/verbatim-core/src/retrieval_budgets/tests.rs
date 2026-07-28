//! Focused unit tests for the retrieval-budgets contract module.

use super::*;

// ---------------------------------------------------------------------------
// MemoryBudgetProfile
// ---------------------------------------------------------------------------

#[test]
fn online_serving_skeleton_is_valid() {
    let p = MemoryBudgetProfile::skeleton_online_serving();
    assert_eq!(p.role(), MemoryProfileRole::OnlineServing);
    assert_eq!(p.high(), 192 * memory::MIB);
    assert_eq!(p.max(), 256 * memory::MIB);
    assert!(p.validate().is_ok());
}

#[test]
fn isolated_build_skeleton_is_valid() {
    let p = MemoryBudgetProfile::skeleton_isolated_build();
    assert_eq!(p.role(), MemoryProfileRole::IsolatedBuild);
    assert!(p.validate().is_ok());
}

#[test]
fn high_not_below_max_rejected() {
    let err = MemoryBudgetProfile::new(MemoryBudgetProfileFields {
        role: MemoryProfileRole::OnlineServing,
        current: 0,
        high: 256 * memory::MIB,
        max: 256 * memory::MIB,
    })
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::MemoryHighNotBelowMax
    );
}

#[test]
fn max_above_high_rejected() {
    let err = MemoryBudgetProfile::new(MemoryBudgetProfileFields {
        role: MemoryProfileRole::OnlineServing,
        current: 0,
        high: 300 * memory::MIB,
        max: 256 * memory::MIB,
    })
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::MemoryHighNotBelowMax
    );
}

#[test]
fn max_below_floor_rejected() {
    let err = MemoryBudgetProfile::new(MemoryBudgetProfileFields {
        role: MemoryProfileRole::OnlineServing,
        current: 0,
        high: 32 * memory::MIB,
        max: 48 * memory::MIB,
    })
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::MemoryMaxBelowFloor
    );
}

#[test]
fn check_current_within_high_ok() {
    let p = MemoryBudgetProfile::skeleton_online_serving();
    assert!(p.check_current(100 * memory::MIB).is_ok());
}

#[test]
fn check_current_between_high_and_max_yields_high_exceeded() {
    let p = MemoryBudgetProfile::skeleton_online_serving();
    let err = p.check_current(200 * memory::MIB).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::MemoryHighExceeded
    );
}

#[test]
fn check_current_over_max_yields_max_exceeded() {
    let p = MemoryBudgetProfile::skeleton_online_serving();
    let err = p.check_current(300 * memory::MIB).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::MemoryMaxExceeded
    );
}

// ---------------------------------------------------------------------------
// PerQueryMemoryCaps
// ---------------------------------------------------------------------------

#[test]
fn per_query_caps_skeleton_valid() {
    let caps = PerQueryMemoryCaps::skeleton_default();
    assert!(caps.validate().is_ok());
    assert!(caps.total().unwrap() > 0);
}

#[test]
fn per_query_caps_zero_field_rejected() {
    let mut f = PerQueryMemoryCaps::skeleton_default().fields();
    f.read_buffers = 0;
    let err = PerQueryMemoryCaps::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidMemoryBudget
    );
}

// ---------------------------------------------------------------------------
// CacheCapacity
// ---------------------------------------------------------------------------

#[test]
fn cache_capacity_skeleton_valid() {
    let cap = CacheCapacity::skeleton_page_cache();
    assert_eq!(cap.kind(), CacheKind::PageCache);
    assert!(cap.validate().is_ok());
    assert!(cap.max_entries() > 0);
}

#[test]
fn cache_capacity_zero_bytes_rejected() {
    let err = CacheCapacity::new(CacheCapacityFields {
        kind: CacheKind::PageCache,
        max_bytes: 0,
        entry_bytes: 4_096,
    })
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidCacheCapacity
    );
}

#[test]
fn cache_capacity_below_one_entry_rejected() {
    let err = CacheCapacity::new(CacheCapacityFields {
        kind: CacheKind::GraphCache,
        max_bytes: 1_000,
        entry_bytes: 4_096,
    })
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::CacheCapacityBelowOneEntry
    );
}

#[test]
fn cache_capacity_admits_check() {
    let cap = CacheCapacity::skeleton_page_cache();
    assert!(cap.admits(32 * 1024));
    assert!(!cap.admits(cap.max_bytes() + 1));
}

#[test]
fn all_cache_kinds_covered() {
    assert_eq!(CacheKind::ALL.len(), 6);
    for k in CacheKind::ALL {
        assert!(!k.as_str().is_empty());
    }
}

// ---------------------------------------------------------------------------
// IoBudget
// ---------------------------------------------------------------------------

#[test]
fn io_budget_skeleton_valid() {
    let b = IoBudget::skeleton_default();
    assert!(b.validate().is_ok());
    assert_eq!(b.access_mode(), IoAccessMode::Direct);
}

#[test]
fn io_budget_zero_pages_rejected() {
    let mut f = IoBudget::skeleton_default().fields();
    f.max_pages = 0;
    let err = IoBudget::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidIoBudget
    );
}

#[test]
fn io_budget_zero_queue_depth_rejected() {
    let mut f = IoBudget::skeleton_default().fields();
    f.max_queue_depth = 0;
    let err = IoBudget::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidQueueDepth
    );
}

#[test]
fn io_budget_zero_read_amp_rejected() {
    let mut f = IoBudget::skeleton_default().fields();
    f.max_read_amplification = 0;
    let err = IoBudget::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidReadAmplificationBound
    );
}

#[test]
fn io_budget_exhaustion_pages() {
    let b = IoBudget::skeleton_default();
    let exhausted = b.exhaustion(b.fields().max_pages + 1, 0, 0, 0, 0).unwrap();
    assert_eq!(exhausted, ResourceExhaustion::PageBudgetExceeded);
}

#[test]
fn io_budget_exhaustion_bytes() {
    let b = IoBudget::skeleton_default();
    let exhausted = b.exhaustion(1, b.fields().max_bytes + 1, 0, 0, 0).unwrap();
    assert_eq!(exhausted, ResourceExhaustion::ByteBudgetExceeded);
}

#[test]
fn io_budget_exhaustion_iops() {
    let b = IoBudget::skeleton_default();
    let exhausted = b.exhaustion(1, 1, b.fields().max_iops + 1, 0, 0).unwrap();
    assert_eq!(exhausted, ResourceExhaustion::IopsExceeded);
}

#[test]
fn io_budget_exhaustion_await() {
    let b = IoBudget::skeleton_default();
    let exhausted = b
        .exhaustion(1, 1, 1, b.fields().max_await_micros + 1, 0)
        .unwrap();
    assert_eq!(exhausted, ResourceExhaustion::AwaitExceeded);
}

#[test]
fn io_budget_exhaustion_read_amplification() {
    let b = IoBudget::skeleton_default();
    let exhausted = b
        .exhaustion(1, 1, 1, 1, b.fields().max_read_amplification + 1)
        .unwrap();
    assert_eq!(exhausted, ResourceExhaustion::ReadAmplificationExceeded);
}

#[test]
fn io_budget_no_exhaustion_when_within_bounds() {
    let b = IoBudget::skeleton_default();
    assert!(b.exhaustion(1, 1, 1, 1, 1).is_none());
}

#[test]
fn io_budget_check_ok() {
    let b = IoBudget::skeleton_default();
    assert!(b.check(1, 1, 1, 1, 1).is_ok());
}

#[test]
fn io_budget_check_err_typed() {
    let b = IoBudget::skeleton_default();
    let err = b.check(b.fields().max_pages + 1, 0, 0, 0, 0).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::PageBudgetExceeded
    );
}

// ---------------------------------------------------------------------------
// ConcurrencyBudget
// ---------------------------------------------------------------------------

#[test]
fn concurrency_skeleton_valid() {
    let b = ConcurrencyBudget::skeleton_default();
    assert!(b.validate().is_ok());
}

#[test]
fn concurrency_zero_queries_rejected() {
    let mut f = ConcurrencyBudget::skeleton_default().fields();
    f.max_active_queries = 0;
    let err = ConcurrencyBudget::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidConcurrencyBudget
    );
}

#[test]
fn concurrency_category_exceeds_workers_rejected() {
    let mut f = ConcurrencyBudget::skeleton_default().fields();
    f.model = f.max_workers + 1;
    let err = ConcurrencyBudget::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::CategoryLimitExceedsWorkers
    );
}

#[test]
fn concurrency_admit_within_limit_ok() {
    let b = ConcurrencyBudget::skeleton_default();
    assert!(b.admit(WorkCategory::Retrieval, 1, 1).is_ok());
}

#[test]
fn concurrency_admit_category_saturated() {
    let b = ConcurrencyBudget::skeleton_default();
    let retrieval_limit = b.category_limit(WorkCategory::Retrieval);
    let err = b
        .admit(WorkCategory::Retrieval, retrieval_limit, 0)
        .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::ConcurrencySaturated
    );
}

#[test]
fn concurrency_admit_total_saturated() {
    let b = ConcurrencyBudget::skeleton_default();
    let err = b
        .admit(WorkCategory::Storage, 0, b.fields().max_workers)
        .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::ConcurrencySaturated
    );
}

#[test]
fn all_work_categories_covered() {
    assert_eq!(WorkCategory::ALL.len(), 4);
}

// ---------------------------------------------------------------------------
// ProcessIsolationSpec
// ---------------------------------------------------------------------------

#[test]
fn online_serving_isolation_valid() {
    let s = ProcessIsolationSpec::skeleton_online_serving();
    assert!(s.validate().is_ok());
    assert!(!s.is_build_isolated());
}

#[test]
fn isolated_build_spec_valid_and_isolated() {
    let s = ProcessIsolationSpec::skeleton_isolated_build();
    assert!(s.validate().is_ok());
    assert!(s.is_build_isolated());
}

#[test]
fn isolation_zero_slice_id_rejected() {
    let mut f = ProcessIsolationSpec::skeleton_online_serving().fields();
    f.cgroup_slice_id = 0;
    let err = ProcessIsolationSpec::new(f).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::InvalidProcessIsolation
    );
}

// ---------------------------------------------------------------------------
// ResourceAccount
// ---------------------------------------------------------------------------

#[test]
fn resource_account_charges_within_budget_ok() {
    let mut acct = ResourceAccount::new(IoBudget::skeleton_default());
    assert!(acct.charge_read(10, 4096, 100, 1).is_ok());
    assert_eq!(acct.pages(), 10);
    assert_eq!(acct.bytes(), 4096);
    assert_eq!(acct.iops(), 1);
    assert!(acct.exhaustion().is_none());
}

#[test]
fn resource_account_page_exhaustion_typed() {
    let mut acct = ResourceAccount::new(IoBudget::skeleton_default());
    let max_pages = acct.budget().fields().max_pages;
    let err = acct.charge_read(max_pages + 1, 1, 1, 1).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::PageBudgetExceeded
    );
    assert_eq!(
        acct.exhaustion(),
        Some(ResourceExhaustion::PageBudgetExceeded)
    );
}

#[test]
fn resource_account_fails_fast_after_exhaustion() {
    let mut acct = ResourceAccount::new(IoBudget::skeleton_default());
    let max_pages = acct.budget().fields().max_pages;
    let _ = acct.charge_read(max_pages + 1, 1, 1, 1);
    let err = acct.charge_read(1, 1, 1, 1).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        RetrievalBudgetDiagnosticCode::PageBudgetExceeded
    );
}

#[test]
fn resource_account_check_ok_when_unexhausted() {
    let mut acct = ResourceAccount::new(IoBudget::skeleton_default());
    assert!(acct.charge_read(1, 1, 1, 1).is_ok());
    assert!(acct.check().is_ok());
}

// ---------------------------------------------------------------------------
// ResourceExhaustion + diagnostic coverage
// ---------------------------------------------------------------------------

#[test]
fn all_resource_exhaustion_variants_have_codes() {
    assert_eq!(ResourceExhaustion::ALL.len(), 9);
    for variant in ResourceExhaustion::ALL {
        let code: RetrievalBudgetDiagnosticCode = variant.into();
        // every variant maps to a distinct non-empty code string
        assert!(!code.as_str().is_empty());
        assert!(!variant.as_str().is_empty());
    }
}

#[test]
fn error_debug_is_redacted_code_only() {
    let err = RetrievalBudgetError::new(RetrievalBudgetDiagnosticCode::PageBudgetExceeded);
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("page_budget_exceeded"));
    // Debug must not contain any payload placeholder beyond the code name
    assert!(!dbg.contains('{'));
}

#[test]
fn error_display_prefixed() {
    let err = RetrievalBudgetError::new(RetrievalBudgetDiagnosticCode::MemoryMaxExceeded);
    let disp = format!("{}", err);
    assert!(disp.starts_with("retrieval-budget."));
    assert!(disp.contains("memory_max_exceeded"));
}

#[test]
fn all_diagnostic_codes_have_stable_strings() {
    for code in RetrievalBudgetDiagnosticCode::ALL {
        assert!(!code.as_str().is_empty());
    }
}

// ---------------------------------------------------------------------------
// IoAccessMode coverage
// ---------------------------------------------------------------------------

#[test]
fn all_io_access_modes_covered() {
    assert_eq!(IoAccessMode::ALL.len(), 3);
    for m in IoAccessMode::ALL {
        assert!(!m.as_str().is_empty());
    }
}
