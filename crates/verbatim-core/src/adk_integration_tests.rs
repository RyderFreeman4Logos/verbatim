use crate::adk_integration::*;

fn policy() -> AdkIntegrationPolicy {
    AdkIntegrationPolicy
}

#[test]
fn standard_catalog_covers_every_adk_crate_with_the_policy_disposition() {
    let catalog = AdkCrateCatalog::standard();
    let expected = [
        (AdkCrateName::Core, AdkCrateDisposition::Adopt),
        (AdkCrateName::Agent, AdkCrateDisposition::Adopt),
        (AdkCrateName::Model, AdkCrateDisposition::Wrap),
        (AdkCrateName::Tool, AdkCrateDisposition::Adopt),
        (AdkCrateName::Runner, AdkCrateDisposition::Adopt),
        (AdkCrateName::Graph, AdkCrateDisposition::Adopt),
        (AdkCrateName::Session, AdkCrateDisposition::Wrap),
        (AdkCrateName::Artifact, AdkCrateDisposition::Wrap),
        (AdkCrateName::Auth, AdkCrateDisposition::Wrap),
        (AdkCrateName::Telemetry, AdkCrateDisposition::Wrap),
        (AdkCrateName::Guardrail, AdkCrateDisposition::Adopt),
        (AdkCrateName::Eval, AdkCrateDisposition::Adopt),
        (AdkCrateName::Rag, AdkCrateDisposition::Wrap),
        (AdkCrateName::Memory, AdkCrateDisposition::Keep),
        (AdkCrateName::Server, AdkCrateDisposition::Keep),
        (AdkCrateName::Action, AdkCrateDisposition::Wrap),
        (AdkCrateName::Sandbox, AdkCrateDisposition::Upstream),
        (AdkCrateName::Mistralrs, AdkCrateDisposition::Keep),
    ];

    assert_eq!(expected.len(), AdkCrateName::ALL.len());
    assert_eq!(catalog.entries().len(), expected.len());
    for (crate_name, disposition) in expected {
        let entry = catalog.entry(crate_name).expect("standard catalog entry");
        assert_eq!(entry.disposition(), disposition);
        assert!(!entry.constraints().is_empty());
    }
    policy()
        .validate_disposition(&catalog)
        .expect("standard catalog validates");
}

#[test]
fn only_supported_dispositions_decode_and_are_reachable_in_the_standard_catalog() {
    assert!(
        serde_json::from_str::<AdkCrateDisposition>("\"delete\"").is_err(),
        "deleting upstream ADK crates is not an integration disposition"
    );

    let catalog = AdkCrateCatalog::standard();
    for disposition in [
        AdkCrateDisposition::Adopt,
        AdkCrateDisposition::Wrap,
        AdkCrateDisposition::Upstream,
        AdkCrateDisposition::Keep,
    ] {
        assert!(
            catalog
                .entries()
                .iter()
                .any(|entry| entry.disposition() == disposition),
            "{disposition:?} must be reachable in the standard catalog"
        );
    }
}

#[test]
fn catalog_validates_before_and_after_a_json_round_trip() {
    let catalog = AdkCrateCatalog::standard();
    policy()
        .validate_disposition(&catalog)
        .expect("catalog validates before encoding");

    let encoded = encode_adk_crate_catalog_json(&catalog).expect("catalog encodes");
    let decoded = decode_adk_crate_catalog_json(&encoded).expect("catalog decodes");
    assert_eq!(decoded, catalog);
    policy()
        .validate_disposition(&decoded)
        .expect("catalog validates after decoding");
}

#[test]
fn disposition_and_constraint_validation_rejects_non_policy_catalog_entries() {
    let catalog = AdkCrateCatalog::standard();
    for disallowed in [
        AdkCrateDisposition::Wrap,
        AdkCrateDisposition::Upstream,
        AdkCrateDisposition::Keep,
    ] {
        let mut wrong_disposition = serde_json::to_value(catalog.clone()).expect("catalog value");
        let entries = wrong_disposition["entries"]
            .as_array_mut()
            .expect("serialized entries array");
        let core = entries
            .iter_mut()
            .find(|entry| entry["crate_name"] == "adk-core")
            .expect("core entry");
        core["disposition"] = serde_json::to_value(disallowed).expect("disposition value");

        let altered: AdkCrateCatalog =
            serde_json::from_value(wrong_disposition).expect("altered catalog decodes");
        let error = policy()
            .validate_disposition(&altered)
            .expect_err("core must retain its adopted disposition");
        assert_eq!(
            error.diagnostic_code(),
            AdkIntegrationDiagnosticCode::CatalogDispositionInvalid
        );
    }

    let mut missing_constraint = serde_json::to_value(catalog).expect("catalog value");
    let entries = missing_constraint["entries"]
        .as_array_mut()
        .expect("serialized entries array");
    let core = entries
        .iter_mut()
        .find(|entry| entry["crate_name"] == "adk-core")
        .expect("core entry");
    core["constraints"] = serde_json::json!([]);

    let altered: AdkCrateCatalog =
        serde_json::from_value(missing_constraint).expect("altered catalog decodes");
    let error = policy()
        .validate_disposition(&altered)
        .expect_err("core constraints must be complete");
    assert_eq!(
        error.diagnostic_code(),
        AdkIntegrationDiagnosticCode::CatalogConstraintsInvalid
    );
}

#[test]
fn sandbox_adoption_requires_verified_platform_security_conformance() {
    let catalog = AdkCrateCatalog::standard();
    let mut encoded = serde_json::to_value(catalog).expect("catalog value");
    {
        let entries = encoded["entries"]
            .as_array_mut()
            .expect("serialized entries array");
        let sandbox = entries
            .iter_mut()
            .find(|entry| entry["crate_name"] == "adk-sandbox")
            .expect("sandbox entry");
        sandbox["disposition"] = serde_json::json!("adopt");
    }

    let pending: AdkCrateCatalog =
        serde_json::from_value(encoded.clone()).expect("pending catalog");
    let error = policy()
        .validate_disposition(&pending)
        .expect_err("unverified sandbox adoption must fail closed");
    assert_eq!(
        error.diagnostic_code(),
        AdkIntegrationDiagnosticCode::SandboxSecurityConformanceRequired
    );

    {
        let entries = encoded["entries"]
            .as_array_mut()
            .expect("serialized entries array");
        let sandbox = entries
            .iter_mut()
            .find(|entry| entry["crate_name"] == "adk-sandbox")
            .expect("sandbox entry");
        sandbox["security_conformance"] = serde_json::json!("verified");
    }
    let verified: AdkCrateCatalog = serde_json::from_value(encoded).expect("verified catalog");
    policy()
        .validate_disposition(&verified)
        .expect("verified sandbox conformance permits adoption");
}

#[test]
fn every_domain_boundary_rule_rejects_its_forbidden_use() {
    let cases = [
        (
            DomainBoundaryRule::PersistedArtifacts,
            AdkBoundaryUse::AdkOnlyArtifactSchema,
            AdkIntegrationDiagnosticCode::ArtifactSchemaBoundaryForbidden,
        ),
        (
            DomainBoundaryRule::PublicWireApi,
            AdkBoundaryUse::AdkInternalWireStruct,
            AdkIntegrationDiagnosticCode::WireSchemaBoundaryForbidden,
        ),
        (
            DomainBoundaryRule::BuiltInWorkflowStorage,
            AdkBoundaryUse::DirectStorageAccess,
            AdkIntegrationDiagnosticCode::WorkflowStorageBoundaryForbidden,
        ),
        (
            DomainBoundaryRule::AgentToolScopeAcl,
            AdkBoundaryUse::AgentToolScopeReplacingSourceChunkAcl,
            AdkIntegrationDiagnosticCode::ScopeAclBoundaryForbidden,
        ),
        (
            DomainBoundaryRule::WorkflowGraphKnowledgeGraph,
            AdkBoundaryUse::AdkGraphReplacingGraphRagKnowledgeGraph,
            AdkIntegrationDiagnosticCode::GraphKnowledgeBoundaryForbidden,
        ),
    ];

    assert_eq!(cases.len(), DomainBoundaryRule::ALL.len());
    for (rule, attempted_use, expected_code) in cases {
        assert_eq!(rule.forbidden_use(), attempted_use);
        let error = policy()
            .check_boundary(attempted_use)
            .expect_err("forbidden ADK crossing must fail closed");
        assert_eq!(error.diagnostic_code(), expected_code);
    }
    policy()
        .check_boundary(AdkBoundaryUse::StableVerbatimAdapter)
        .expect("stable Verbatim adapter is the allowed boundary");
}

#[test]
fn only_an_exact_stable_one_x_version_is_accepted_and_round_trips() {
    let contract = policy();
    let version = contract.pin_version("1.0.0").expect("exact stable release");
    assert_eq!(version.to_string(), "1.0.0");

    let encoded = encode_version_policy_json(&version).expect("version policy encodes");
    let decoded = decode_version_policy_json(&encoded).expect("version policy decodes");
    assert_eq!(decoded, version);

    for invalid in [
        "1",
        "1.0",
        "^1.0.0",
        "~1.0.0",
        "1.0.0-beta.1",
        "1.0.0+build.1",
        "git+https://example.invalid/adk-rust",
        "main",
        "0.9.0",
        "2.0.0",
    ] {
        let error = contract
            .pin_version(invalid)
            .expect_err("floating, non-stable, or non-1.x versions must fail");
        assert_eq!(
            error.diagnostic_code(),
            AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX
        );
    }
}

#[test]
fn integration_errors_render_only_closed_diagnostic_codes() {
    let untrusted_input = "token=adk-secret-value";
    let error = decode_adk_crate_catalog_json(untrusted_input).expect_err("invalid JSON fails");

    assert_eq!(
        format!("{error:?}"),
        "AdkIntegrationError(invalid_catalog_json)"
    );
    assert_eq!(error.to_string(), "adk-integration.invalid_catalog_json");
    assert!(!format!("{error:?}").contains(untrusted_input));
    assert!(!error.to_string().contains(untrusted_input));
}
