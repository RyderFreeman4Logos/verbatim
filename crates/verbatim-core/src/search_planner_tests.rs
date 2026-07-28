use crate::search_planner::{
    SearchBudget, SearchBudgetFields, SearchBudgetUsage, SearchPlannerDiagnosticCode,
};

fn valid_budget_fields() -> SearchBudgetFields {
    SearchBudgetFields {
        result_limit: 10,
        dense_candidate_limit: 64,
        lexical_candidate_limit: 64,
        exact_candidate_limit: 32,
        graph_candidate_limit: 32,
        fused_pool_limit: 128,
        rerank_candidate_limit: 64,
        full_precision_rescore_limit: 32,
        hydration_limit: 10,
        max_ssd_pages: 16,
        max_bytes_read: 1_048_576,
        max_cpu_micros: 50_000,
        max_work_units: 10_000,
        max_wall_time_micros: 100_000,
        max_concurrent_stages: 2,
        max_stage_attempts: 3,
        debug_record_limit: 16,
    }
}

#[test]
fn search_budget_rejects_zero_caps() {
    let mut fields = valid_budget_fields();
    fields.result_limit = 0;

    let code = SearchBudget::new(fields)
        .err()
        .map(|error| error.diagnostic_code());

    assert_eq!(code, Some(SearchPlannerDiagnosticCode::BudgetExceeded));
}

#[test]
fn tight_budgets_reject_overflow_widening_and_every_excess_dimension() {
    let mut overflowing_fields = valid_budget_fields();
    overflowing_fields.dense_candidate_limit = u32::MAX;
    overflowing_fields.lexical_candidate_limit = 1;
    let overflow_code = SearchBudget::new(overflowing_fields)
        .err()
        .map(|error| error.diagnostic_code());
    assert_eq!(
        overflow_code,
        Some(SearchPlannerDiagnosticCode::BudgetOverflow)
    );

    let requested = match SearchBudget::new(valid_budget_fields()) {
        Ok(budget) => budget,
        Err(error) => panic!("unexpected requested-budget error: {error:?}"),
    };
    let mut widened_fields = requested.fields();
    widened_fields.max_bytes_read += 1;
    let widened = match SearchBudget::new(widened_fields) {
        Ok(budget) => budget,
        Err(error) => panic!("unexpected widened-budget construction error: {error:?}"),
    };
    let widening_code = widened
        .ensure_not_wider_than(&requested)
        .err()
        .map(|error| error.diagnostic_code());
    assert_eq!(
        widening_code,
        Some(SearchPlannerDiagnosticCode::PlanBudgetWidened)
    );

    let mut tight_fields = valid_budget_fields();
    tight_fields.result_limit = 1;
    tight_fields.dense_candidate_limit = 1;
    tight_fields.lexical_candidate_limit = 1;
    tight_fields.exact_candidate_limit = 1;
    tight_fields.graph_candidate_limit = 1;
    tight_fields.fused_pool_limit = 1;
    tight_fields.rerank_candidate_limit = 1;
    tight_fields.full_precision_rescore_limit = 1;
    tight_fields.hydration_limit = 1;
    tight_fields.max_ssd_pages = 1;
    tight_fields.max_bytes_read = 1;
    tight_fields.max_cpu_micros = 1;
    tight_fields.max_work_units = 1;
    tight_fields.max_wall_time_micros = 1;
    tight_fields.max_concurrent_stages = 1;
    tight_fields.max_stage_attempts = 1;
    tight_fields.debug_record_limit = 1;
    let tight = match SearchBudget::new(tight_fields) {
        Ok(budget) => budget,
        Err(error) => panic!("unexpected tight-budget error: {error:?}"),
    };

    let excessive_usages = [
        SearchBudgetUsage {
            result_records: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            dense_candidates: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            lexical_candidates: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            exact_candidates: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            graph_candidates: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            fused_records: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            reranked_records: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            full_precision_rescored_records: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            hydrated_records: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            ssd_pages_read: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            bytes_read: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            cpu_micros: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            work_units: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            wall_time_micros: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            concurrent_stages: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            stage_attempts: 2,
            ..SearchBudgetUsage::default()
        },
        SearchBudgetUsage {
            debug_records: 2,
            ..SearchBudgetUsage::default()
        },
    ];
    for usage in excessive_usages {
        let code = tight
            .validate_usage(usage)
            .err()
            .map(|error| error.diagnostic_code());
        assert_eq!(code, Some(SearchPlannerDiagnosticCode::ActualWorkExceeded));
    }
}
