//! Schema-constrained selection of persisted evidence ids.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::answer::{evidence_ids_only_allowed, AnswerPlan};
use super::error::{WorkflowError, WorkflowResult};
use super::policy::WorkflowPolicyContext;
use super::run::WorkflowRun;
use super::workflow::{fail_closed, WorkflowOutcome};
use crate::store::Store;
use crate::types::report_artifact::is_report_artifact_id;
use crate::types::{EvidenceId, EvidenceKind};

/// Testable boundary for a schema-constrained selector response.
pub trait EvidenceIdSelector {
    /// Return only the raw schema-constrained selector response.
    fn select_evidence_ids(&self, plan: &AnswerPlan) -> WorkflowResult<String>;
}

/// Result of one selection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceSelectionResult {
    /// Persisted evidence ids selected from the plan allowlist.
    Selected { evidence_ids: Vec<EvidenceId> },
    /// A fail-closed terminal workflow outcome.
    Terminal(Box<WorkflowOutcome>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectorResponse {
    decision: SelectorDecision,
    selected_evidence_ids: Vec<String>,
    #[serde(default)]
    missing_requirements: Vec<String>,
    #[serde(default)]
    conflicts: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SelectorDecision {
    Select,
    Abstain,
}

/// Select only persisted, allowlisted evidence ids or return a terminal failure.
///
/// The adapter never exposes evidence text; deterministic citation rendering
/// remains the sole publisher. Every selector, schema, policy, allowlist, or
/// store failure becomes the existing typed fail-closed outcome.
pub fn select_persisted_evidence(
    run: WorkflowRun,
    policy: &WorkflowPolicyContext,
    plan: &AnswerPlan,
    selector: &dyn EvidenceIdSelector,
    store: &Store,
) -> WorkflowResult<EvidenceSelectionResult> {
    let selection = (|| {
        policy.require_model_enabled()?;
        let response =
            serde_json::from_str::<SelectorResponse>(&selector.select_evidence_ids(plan)?)
                .map_err(|error| {
                    WorkflowError::verification_failed(format!("invalid selector JSON: {error}"))
                })?;
        validate_selection(response, plan, store)
    })();

    match selection {
        Ok(evidence_ids) => Ok(EvidenceSelectionResult::Selected { evidence_ids }),
        Err(error) => Ok(EvidenceSelectionResult::Terminal(Box::new(fail_closed(
            run, error,
        )?))),
    }
}

fn validate_selection(
    response: SelectorResponse,
    plan: &AnswerPlan,
    store: &Store,
) -> WorkflowResult<Vec<EvidenceId>> {
    match response.decision {
        SelectorDecision::Abstain => {
            if response.selected_evidence_ids.is_empty() {
                return Err(WorkflowError::verification_failed("selector abstained"));
            }
            return Err(WorkflowError::verification_failed(
                "abstaining selector must not select evidence ids",
            ));
        }
        SelectorDecision::Select => {}
    }
    if response.selected_evidence_ids.is_empty() {
        return Err(WorkflowError::verification_failed(
            "selector must select at least one evidence id",
        ));
    }
    if !response.missing_requirements.is_empty() || !response.conflicts.is_empty() {
        return Err(WorkflowError::verification_failed(
            "selector reported missing requirements or conflicts",
        ));
    }
    if response
        .selected_evidence_ids
        .iter()
        .any(|id| is_report_artifact_id(id))
    {
        return Err(WorkflowError::missing_evidence(
            "selector chose a report artifact id",
        ));
    }
    if !evidence_ids_only_allowed(plan, &response.selected_evidence_ids) {
        return Err(WorkflowError::missing_evidence(
            "selector chose an id outside the answer plan allowlist",
        ));
    }
    let ids = response
        .selected_evidence_ids
        .into_iter()
        .map(EvidenceId)
        .collect::<Vec<_>>();
    let unique_ids = ids.iter().map(|id| id.0.as_str()).collect::<BTreeSet<_>>();
    if unique_ids.len() != ids.len() {
        return Err(WorkflowError::verification_failed(
            "selector chose duplicate evidence ids",
        ));
    }
    let evidence = store
        .get_evidence_batch(&ids)
        .map_err(|error| WorkflowError::missing_evidence(error.to_string()))?;
    if evidence.len() != ids.len() || ids.iter().any(|id| {
        !matches!(
            evidence.get(id),
            Some(Ok(evidence)) if matches!(evidence.kind, EvidenceKind::Text | EvidenceKind::Image)
        )
    }) {
        return Err(WorkflowError::missing_evidence(
            "selector chose unknown, unreadable, or non-citable evidence",
        ));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::super::answer::AnswerPlanFields;
    use super::super::policy::WorkflowPolicyContextFields;
    use super::super::run::WorkflowRunFields;
    use super::*;

    struct StaticEvidenceSelector(&'static str);

    impl EvidenceIdSelector for StaticEvidenceSelector {
        fn select_evidence_ids(&self, _: &AnswerPlan) -> WorkflowResult<String> {
            Ok(self.0.into())
        }
    }

    struct FailingEvidenceSelector;

    impl EvidenceIdSelector for FailingEvidenceSelector {
        fn select_evidence_ids(&self, _: &AnswerPlan) -> WorkflowResult<String> {
            Err(WorkflowError::verification_failed("selector timed out"))
        }
    }

    fn policy(model_enabled: bool) -> WorkflowPolicyContext {
        WorkflowPolicyContext::new(WorkflowPolicyContextFields {
            principal: "user:alice".into(),
            profile_ref: "profile:default".into(),
            policy_version: "policy-v1".into(),
            model_enabled,
            remaining_revisions: 1,
            remaining_cost_units: None,
        })
        .unwrap()
    }

    fn plan(allowed_evidence_unit_ids: Vec<String>) -> AnswerPlan {
        AnswerPlan::new(AnswerPlanFields {
            plan_id: "plan-selection".into(),
            context_pack_hash: "context-hash-1".into(),
            instruction: "Select only evidence ids.".into(),
            allowed_evidence_unit_ids,
            max_claims: 1,
            model_fingerprint: None,
        })
        .unwrap()
    }

    fn run() -> WorkflowRun {
        WorkflowRun::new(WorkflowRunFields {
            run_id: "run-selection".into(),
            query_plan_hash: "query-plan-hash".into(),
            profile_ref: None,
            generation: None,
        })
        .unwrap()
    }

    fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(&directory.path().join("verbatim.db")).unwrap();
        (directory, store)
    }

    fn assert_abstained(result: EvidenceSelectionResult) {
        assert!(matches!(
            result,
            EvidenceSelectionResult::Terminal(outcome) if outcome.is_abstained()
        ));
    }

    fn persist_evidence(store: &Store, id: &str, kind: crate::types::EvidenceKind) {
        let source = crate::types::Source {
            id: crate::types::SourceId(format!("source-{id}")),
            path: std::path::PathBuf::from(format!("/tmp/{id}.txt")),
            hash: format!("source-hash-{id}"),
            status: crate::types::SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        };
        store.add_source(&source).unwrap();
        store
            .bulk_insert_evidence(&[crate::types::EvidenceUnit {
                id: EvidenceId(id.into()),
                source_id: source.id,
                kind,
                derived_from: None,
                locator: crate::types::SourceLocator::Document {
                    path_or_url: source.path.to_string_lossy().into_owned(),
                    line_start: 1,
                    line_end: None,
                },
                text: "persisted evidence".into(),
                text_hash: format!("evidence-hash-{id}"),
                heading_path: Vec::new(),
                language: None,
                position: 0,
            }])
            .unwrap();
    }

    #[test]
    fn allowlisted_non_citable_kinds_abstain() {
        let (_directory, store) = store();
        persist_evidence(&store, "eu-ocr", crate::types::EvidenceKind::Ocr);
        persist_evidence(
            &store,
            "eu-generated",
            crate::types::EvidenceKind::Generated,
        );

        for (id, response) in [
            (
                "eu-ocr",
                "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-ocr\"]}",
            ),
            (
                "eu-generated",
                "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-generated\"]}",
            ),
        ] {
            assert_abstained(
                select_persisted_evidence(
                    run(),
                    &policy(true),
                    &plan(vec![id.into()]),
                    &StaticEvidenceSelector(response),
                    &store,
                )
                .unwrap(),
            );
        }
    }

    #[test]
    fn allowlisted_persisted_citable_kinds_are_selected() {
        let (_directory, store) = store();
        persist_evidence(&store, "eu-text", crate::types::EvidenceKind::Text);
        persist_evidence(&store, "eu-image", crate::types::EvidenceKind::Image);

        for (id, response) in [
            (
                "eu-text",
                "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-text\"]}",
            ),
            (
                "eu-image",
                "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-image\"]}",
            ),
        ] {
            assert_eq!(
                select_persisted_evidence(
                    run(),
                    &policy(true),
                    &plan(vec![id.into()]),
                    &StaticEvidenceSelector(response),
                    &store,
                )
                .unwrap(),
                EvidenceSelectionResult::Selected {
                    evidence_ids: vec![EvidenceId(id.into())],
                },
            );
        }
    }

    #[test]
    fn selector_error_abstains() {
        let (_directory, store) = store();
        assert_abstained(
            select_persisted_evidence(
                run(),
                &policy(true),
                &plan(vec!["eu-text".into()]),
                &FailingEvidenceSelector,
                &store,
            )
            .unwrap(),
        );
    }

    #[test]
    fn missing_requirements_or_conflicts_abstain() {
        let (_directory, store) = store();
        persist_evidence(&store, "eu-text", crate::types::EvidenceKind::Text);
        for response in [
            "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-text\"],\"missing_requirements\":[\"citation\"]}",
            "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-text\"],\"conflicts\":[\"contradiction\"]}",
        ] {
            assert_abstained(
                select_persisted_evidence(
                    run(),
                    &policy(true),
                    &plan(vec!["eu-text".into()]),
                    &StaticEvidenceSelector(response),
                    &store,
                )
                .unwrap(),
            );
        }
    }

    #[test]
    fn unknown_id_abstains() {
        let (_directory, store) = store();
        assert_abstained(
            select_persisted_evidence(
                run(),
                &policy(true),
                &plan(vec!["eu-missing".into()]),
                &StaticEvidenceSelector(
                    "{\"decision\":\"select\",\"selected_evidence_ids\":[\"eu-missing\"]}",
                ),
                &store,
            )
            .unwrap(),
        );
    }

    #[test]
    fn report_artifact_id_abstains() {
        let (_directory, store) = store();
        assert_abstained(
            select_persisted_evidence(
                run(),
                &policy(true),
                &plan(vec!["graphrag://report/community-1".into()]),
                &StaticEvidenceSelector(
                    "{\"decision\":\"select\",\"selected_evidence_ids\":[\"graphrag://report/community-1\"]}",
                ),
                &store,
            )
            .unwrap(),
        );
    }

    #[test]
    fn invalid_json_and_free_form_answers_abstain() {
        let (_directory, store) = store();
        for response in ["not json", "A helpful answer"] {
            assert_abstained(
                select_persisted_evidence(
                    run(),
                    &policy(true),
                    &plan(vec!["eu-a".into()]),
                    &StaticEvidenceSelector(response),
                    &store,
                )
                .unwrap(),
            );
        }
    }

    #[test]
    fn disabled_policy_returns_disabled_without_selecting() {
        let (_directory, store) = store();
        let result = select_persisted_evidence(
            run(),
            &policy(false),
            &plan(vec!["eu-a".into()]),
            &StaticEvidenceSelector("not json"),
            &store,
        )
        .unwrap();
        assert!(matches!(
            result,
            EvidenceSelectionResult::Terminal(outcome) if matches!(*outcome, WorkflowOutcome::Disabled { .. })
        ));
    }
}
