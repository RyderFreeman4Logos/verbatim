//! Pure validators for completeness, profile, referential integrity, and hashes.

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};
use crate::types::EmbeddingProfileId;

use super::manifest::{BuildStatus, ComponentKind, IndexPublicationManifest};

/// Single validation issue discovered while checking a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub code: &'static str,
    pub detail: String,
}

impl ValidationIssue {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// Aggregate validation report. Empty issues means pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn ok() -> Self {
        Self { issues: Vec::new() }
    }

    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn push(&mut self, issue: ValidationIssue) {
        self.issues.push(issue);
    }

    pub fn merge(&mut self, other: ValidationReport) {
        self.issues.extend(other.issues);
    }

    /// Convert a failing report into a typed storage error.
    pub fn into_result(self) -> StorageResult<()> {
        if self.is_ok() {
            Ok(())
        } else {
            let detail = self
                .issues
                .iter()
                .map(|i| format!("{}: {}", i.code, i.detail))
                .collect::<Vec<_>>()
                .join("; ");
            Err(StorageError::invalid_request(format!(
                "index publication validation failed: {detail}"
            )))
        }
    }
}

/// Require every declared capability to have a matching component digest.
pub fn validate_completeness(manifest: &IndexPublicationManifest) -> ValidationReport {
    let mut report = ValidationReport::ok();
    for kind in manifest.capabilities.required_kinds() {
        if manifest.component(kind).is_none() {
            report.push(ValidationIssue::new(
                "missing_component",
                format!(
                    "declared capability {} lacks a component digest",
                    kind.as_str()
                ),
            ));
        }
    }
    // Generation fields must align with declared capabilities.
    if manifest.capabilities.lexical && manifest.lexical_generation.is_none() {
        report.push(ValidationIssue::new(
            "missing_lexical_generation",
            "lexical capability requires lexical_generation",
        ));
    }
    if manifest.capabilities.vector && manifest.vector_generation.is_none() {
        report.push(ValidationIssue::new(
            "missing_vector_generation",
            "vector capability requires vector_generation",
        ));
    }
    if manifest.capabilities.graph && manifest.graph_generation.is_none() {
        report.push(ValidationIssue::new(
            "missing_graph_generation",
            "graph capability requires graph_generation",
        ));
    }
    report
}

/// Vector publications must declare a profile; optional expected profile must match.
pub fn validate_profile_compatibility(
    manifest: &IndexPublicationManifest,
    expected_profile: Option<&EmbeddingProfileId>,
) -> ValidationReport {
    let mut report = ValidationReport::ok();
    if manifest.capabilities.vector {
        match &manifest.profile_id {
            None => report.push(ValidationIssue::new(
                "missing_profile",
                "vector capability requires profile_id",
            )),
            Some(actual) => {
                if let Some(expected) = expected_profile {
                    if actual != expected {
                        report.push(ValidationIssue::new(
                            "profile_mismatch",
                            format!(
                                "manifest profile {} does not match expected {}",
                                actual.as_str(),
                                expected.as_str()
                            ),
                        ));
                    }
                }
            }
        }
    }
    report
}

/// Component generations must match the corresponding generation fields.
pub fn validate_referential_integrity(manifest: &IndexPublicationManifest) -> ValidationReport {
    let mut report = ValidationReport::ok();
    check_component_generation(
        &mut report,
        manifest,
        ComponentKind::Evidence,
        Some(manifest.evidence_generation),
    );
    check_component_generation(
        &mut report,
        manifest,
        ComponentKind::Catalog,
        Some(manifest.catalog_generation),
    );
    check_component_generation(
        &mut report,
        manifest,
        ComponentKind::Lexical,
        manifest.lexical_generation,
    );
    check_component_generation(
        &mut report,
        manifest,
        ComponentKind::Vector,
        manifest.vector_generation,
    );
    check_component_generation(
        &mut report,
        manifest,
        ComponentKind::Graph,
        manifest.graph_generation,
    );
    report
}

fn check_component_generation(
    report: &mut ValidationReport,
    manifest: &IndexPublicationManifest,
    kind: ComponentKind,
    expected: Option<StorageGeneration>,
) {
    let Some(component) = manifest.component(kind) else {
        return;
    };
    let Some(expected_gen) = expected else {
        report.push(ValidationIssue::new(
            "orphan_component_generation",
            format!(
                "component {} present without a matching generation field",
                kind.as_str()
            ),
        ));
        return;
    };
    if component.generation != expected_gen {
        report.push(ValidationIssue::new(
            "generation_mismatch",
            format!(
                "component {} generation {} does not match manifest field {}",
                kind.as_str(),
                component.generation,
                expected_gen
            ),
        ));
    }
}

/// Digests must be non-empty and well-formed (no whitespace).
pub fn validate_hash_integrity(manifest: &IndexPublicationManifest) -> ValidationReport {
    let mut report = ValidationReport::ok();
    for snap in &manifest.source_snapshots {
        if let Err(err) = snap.validate() {
            report.push(ValidationIssue::new(
                "source_digest_invalid",
                err.to_string(),
            ));
        }
    }
    for digest in &manifest.component_digests {
        if let Err(err) = digest.validate() {
            report.push(ValidationIssue::new(
                "component_digest_invalid",
                format!("{}: {err}", digest.kind.as_str()),
            ));
        }
    }
    report
}

/// Full promotion gate: structure already assumed; status + completeness +
/// integrity + optional profile expectation.
pub fn validate_for_promotion(
    manifest: &IndexPublicationManifest,
    expected_profile: Option<&EmbeddingProfileId>,
) -> ValidationReport {
    let mut report = ValidationReport::ok();

    if !manifest.status.can_promote() {
        report.push(ValidationIssue::new(
            "status_not_promotable",
            format!(
                "build status {} cannot promote (only ready is eligible)",
                manifest.status.as_str()
            ),
        ));
    }
    if matches!(
        manifest.status,
        BuildStatus::Building | BuildStatus::Validating | BuildStatus::Failed
    ) {
        // Explicit incomplete / failed gate even if can_promote changes later.
        report.push(ValidationIssue::new(
            "incomplete_or_failed",
            format!(
                "generation with status {} must not become active",
                manifest.status.as_str()
            ),
        ));
    }

    report.merge(validate_completeness(manifest));
    report.merge(validate_profile_compatibility(manifest, expected_profile));
    report.merge(validate_referential_integrity(manifest));
    report.merge(validate_hash_integrity(manifest));
    report
}
