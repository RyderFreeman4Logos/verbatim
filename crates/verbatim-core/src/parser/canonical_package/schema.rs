//! Versioned manifest and record contract. Identity defaults remain confined to legacy JSONL.
use super::{diagnostic, CanonicalPackageDiagnostic, MANIFEST};
use crate::profiles::bible::canon_registry::VERSION as CANON_VERSION;
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::types::{BackingSelector, DerivedConversionMetadata};
use serde::{Deserialize, Serialize};
const SUPPORTED_MAJOR: u64 = 1;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Manifest {
    #[serde(default)]
    pub(super) schema_version: String,
    #[serde(default)]
    pub(super) profile: String,
    #[serde(default)]
    pub(super) content_kind: String,
    #[serde(default)]
    pub(super) work_id: String,
    #[serde(default)]
    pub(super) version_id: String,
    #[serde(default)]
    pub(super) canon_id: Option<String>,
    #[serde(default)]
    pub(super) versification_id: Option<String>,
    #[serde(default)]
    pub(super) language: String,
    #[serde(default)]
    pub(super) original_source_hash: Option<String>,
    #[serde(default)]
    pub(super) conversion: Option<DerivedConversionMetadata>,
    #[serde(default)]
    pub(super) original_source: Option<OriginalSource>,
    #[serde(default)]
    pub(super) converter_artifact_hash: Option<String>,
    #[serde(default)]
    pub(super) validation: Option<Validation>,
    #[serde(default)]
    pub(super) rights: Option<Rights>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Unit {
    #[serde(default)]
    pub(super) unit_id: String,
    #[serde(default)]
    pub(super) source_profile: String,
    #[serde(default)]
    pub(super) work_id: String,
    #[serde(default)]
    pub(super) version_id: String,
    #[serde(default)]
    pub(super) canon_id: Option<String>,
    #[serde(default)]
    pub(super) versification_id: Option<String>,
    #[serde(default)]
    pub(super) language: String,
    #[serde(default)]
    pub(super) components: Vec<serde_json::Value>,
    #[serde(default)]
    pub(super) text: String,
    #[serde(default)]
    pub(super) content_kind: String,
    #[serde(default)]
    pub(super) text_hash: Option<String>,
    #[serde(default)]
    pub(super) backing_selectors: Vec<BackingSelector>,
    #[serde(default)]
    pub(super) end_components: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub(super) metadata: serde_json::Value,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct OriginalSource {
    pub format: String,
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Validation {
    pub status: String,
    pub validator: String,
    pub version: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Rights {
    pub license: String,
    pub statement: String,
}

pub(super) fn registry_ids(manifest: &Manifest) -> (&str, &str) {
    (
        manifest.canon_id.as_deref().unwrap_or(CANON_VERSION),
        manifest
            .versification_id
            .as_deref()
            .unwrap_or(VERSIFICATION_VERSION),
    )
}

pub(super) fn validate_manifest(
    manifest: &Manifest,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    for (field, value) in [
        ("schema_version", &manifest.schema_version),
        ("profile", &manifest.profile),
        ("content_kind", &manifest.content_kind),
        ("work_id", &manifest.work_id),
        ("version_id", &manifest.version_id),
        ("language", &manifest.language),
    ] {
        if value.trim().is_empty() {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_MANIFEST_REQUIRED_FIELD",
                location: format!("{MANIFEST}:{field}"),
                message: format!("{field} is required"),
            });
        }
    }
    let (canon_id, versification_id) = registry_ids(manifest);
    if canon_id != CANON_VERSION {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_CANON_UNKNOWN",
            location: format!("{MANIFEST}:canon_id"),
            message: format!("unknown canon_id {canon_id}"),
        });
    }
    if VersificationRegistry::by_id(versification_id).is_none() {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_VERSIFICATION_UNKNOWN",
            location: format!("{MANIFEST}:versification_id"),
            message: format!("unknown versification_id {versification_id}"),
        });
    } else if !VersificationRegistry::compatible_with_canon(canon_id) {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_VERSIFICATION_CANON_MISMATCH",
            location: format!("{MANIFEST}:versification_id"),
            message: format!(
                "versification_id {versification_id} is incompatible with canon_id {canon_id}"
            ),
        });
    }
    match manifest
        .schema_version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u64>().ok())
    {
        Some(SUPPORTED_MAJOR) if is_supported_schema_version(&manifest.schema_version) => {}
        Some(SUPPORTED_MAJOR) => diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_SCHEMA_VERSION_INVALID",
            location: MANIFEST.into(),
            message: "schema_version must match 1.<minor>.<patch>".into(),
        }),
        Some(_) => diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_UNSUPPORTED_SCHEMA_MAJOR",
            location: MANIFEST.into(),
            message: format!("unsupported schema version {}", manifest.schema_version),
        }),
        None if !manifest.schema_version.is_empty() => {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_SCHEMA_VERSION_INVALID",
                location: MANIFEST.into(),
                message: "schema_version must start with a numeric major version".into(),
            })
        }
        None => {}
    }
}

fn is_supported_schema_version(version: &str) -> bool {
    let mut parts = version.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("1"), Some(minor), Some(patch), None)
            if !minor.is_empty()
                && !patch.is_empty()
                && minor.bytes().all(|byte| byte.is_ascii_digit())
                && patch.bytes().all(|byte| byte.is_ascii_digit())
    )
}

pub(super) fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn validate_contract(
    manifest: &Manifest,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    for (field, present) in [
        ("canon_id", manifest.canon_id.is_some()),
        ("versification_id", manifest.versification_id.is_some()),
        ("original_source", manifest.original_source.is_some()),
        (
            "original_source_hash",
            manifest.original_source_hash.is_some(),
        ),
        ("conversion", manifest.conversion.is_some()),
        (
            "converter_artifact_hash",
            manifest.converter_artifact_hash.is_some(),
        ),
        ("validation", manifest.validation.is_some()),
        ("rights", manifest.rights.is_some()),
    ] {
        if !present {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_MANIFEST_REQUIRED_FIELD",
                &format!("{MANIFEST}:{field}"),
                format!("{field} is required"),
            ));
        }
    }
    if manifest.profile != "bible" || manifest.content_kind != "text" {
        diagnostics.push(diagnostic(
            "CANONICAL_PACKAGE_PROFILE_UNSUPPORTED",
            MANIFEST,
            "v1 supports bible packages with text content (verse/text and footnote units)",
        ));
    }
    for (field, hash) in [
        (
            "original_source_hash",
            manifest.original_source_hash.as_deref(),
        ),
        (
            "converter_artifact_hash",
            manifest.converter_artifact_hash.as_deref(),
        ),
    ] {
        if hash.is_some_and(|hash| !sha256(hash)) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_HASH_INVALID",
                &format!("{MANIFEST}:{field}"),
                "expected lowercase SHA-256",
            ));
        }
    }
    if let Some(source) = &manifest.original_source {
        if source.name.trim().is_empty() || source.format.trim().is_empty() {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_SOURCE_INVALID",
                "manifest.json:original_source",
                "source name and format are required",
            ));
        }
    }
    if let Some(conversion) = &manifest.conversion {
        if Some(&conversion.original_source_hash) != manifest.original_source_hash.as_ref()
            || !sha256(&conversion.output_hash)
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_CONVERSION_INVALID",
                "manifest.json:conversion",
                "conversion source hash must match manifest; output_hash must be lowercase SHA-256",
            ));
        }
    }
    if let Some(validation) = &manifest.validation {
        if !matches!(validation.status.as_str(), "passed" | "warnings")
            || validation.validator.trim().is_empty()
            || validation.version.trim().is_empty()
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_VALIDATION_INVALID",
                "manifest.json:validation",
                "validation requires passed/warnings status and validator identity/version",
            ));
        }
    }
    if let Some(rights) = &manifest.rights {
        if rights.license.trim().is_empty() || rights.statement.trim().is_empty() {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_RIGHTS_INVALID",
                "manifest.json:rights",
                "license and rights statement are required (unknown is explicit, not permission)",
            ));
        }
    }
}
