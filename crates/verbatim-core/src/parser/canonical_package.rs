use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::parser::canonical_jsonl::CanonicalJsonlParser;
use crate::traits::Parser;
use crate::types::{hex_sha256, BackingSelector, EvidenceUnit, SourceId};

const MANIFEST: &str = "manifest.json";
const UNITS: &str = "units.jsonl";
const SUPPORTED_MAJOR: u64 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalPackageDiagnostic {
    pub code: &'static str,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalPackageReport {
    pub valid: bool,
    pub schema_version: Option<String>,
    pub unit_count: usize,
    pub diagnostics: Vec<CanonicalPackageDiagnostic>,
    pub report_hash: String,
}

#[derive(Debug, Default, Deserialize)]
struct Manifest {
    #[serde(default)]
    schema_version: String,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    content_kind: String,
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Default, Deserialize)]
struct Unit {
    #[serde(default)]
    source_profile: String,
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    components: Vec<serde_json::Value>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    text_hash: Option<String>,
    #[serde(default)]
    backing_selectors: Vec<BackingSelector>,
}

pub struct CanonicalPackageParser;

impl Parser for CanonicalPackageParser {
    fn name(&self) -> &str {
        "canonical_package"
    }
    fn supported_extensions(&self) -> &[&str] {
        &[]
    }
    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let report = validate_package(path);
        if !report.valid {
            let diagnostic = report
                .diagnostics
                .first()
                .expect("invalid report has a diagnostic");
            bail!(
                "{} at {}: {}",
                diagnostic.code,
                diagnostic.location,
                diagnostic.message
            );
        }
        let mut units = CanonicalJsonlParser.parse(&path.join(UNITS))?;
        let source_id = SourceId::from_path(path);
        for unit in &mut units {
            unit.source_id = source_id.clone();
        }
        Ok(units)
    }
}

pub fn validate_package(path: &Path) -> CanonicalPackageReport {
    let mut diagnostics = Vec::new();
    let manifest_path = path.join(MANIFEST);
    let units_path = path.join(UNITS);
    let manifest = match fs::read_to_string(&manifest_path) {
        Ok(contents) => match serde_json::from_str::<Manifest>(&contents) {
            Ok(manifest) => manifest,
            Err(error) => {
                diagnostics.push(diagnostic(
                    "CANONICAL_PACKAGE_MANIFEST_INVALID",
                    MANIFEST,
                    error,
                ));
                Manifest::default()
            }
        },
        Err(error) => {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_MANIFEST_MISSING",
                MANIFEST,
                error,
            ));
            Manifest::default()
        }
    };
    validate_manifest(&manifest, &mut diagnostics);

    let mut unit_count = 0;
    match fs::File::open(&units_path) {
        Ok(file) => {
            for (index, line) in BufReader::new(file).lines().enumerate() {
                let location = format!("{UNITS}:{}", index + 1);
                let line = match line {
                    Ok(line) if !line.trim().is_empty() => line,
                    Ok(_) => continue,
                    Err(error) => {
                        diagnostics.push(diagnostic(
                            "CANONICAL_PACKAGE_UNITS_READ_FAILED",
                            &location,
                            error,
                        ));
                        continue;
                    }
                };
                unit_count += 1;
                match serde_json::from_str::<Unit>(&line) {
                    Ok(unit) => validate_unit(&unit, &manifest, &location, &mut diagnostics),
                    Err(error) => diagnostics.push(diagnostic(
                        "CANONICAL_PACKAGE_UNIT_INVALID",
                        &location,
                        error,
                    )),
                }
            }
        }
        Err(error) => diagnostics.push(diagnostic("CANONICAL_PACKAGE_UNITS_MISSING", UNITS, error)),
    }
    if unit_count == 0
        && diagnostics
            .iter()
            .all(|item| item.code != "CANONICAL_PACKAGE_UNITS_MISSING")
    {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_UNITS_EMPTY",
            location: UNITS.into(),
            message: "units.jsonl must contain at least one unit".into(),
        });
    }
    let valid = diagnostics.is_empty();
    let payload = serde_json::json!({"valid": valid, "schema_version": manifest.schema_version, "unit_count": unit_count, "diagnostics": diagnostics});
    let report_hash = hex_sha256(
        serde_json::to_string(&payload)
            .expect("report serializes")
            .as_bytes(),
    );
    CanonicalPackageReport {
        valid,
        schema_version: (!manifest.schema_version.is_empty()).then_some(manifest.schema_version),
        unit_count,
        diagnostics,
        report_hash,
    }
}

pub fn package_hash(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for name in [MANIFEST, UNITS] {
        let bytes =
            fs::read(path.join(name)).with_context(|| format!("read package file {name}"))?;
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_manifest(manifest: &Manifest, diagnostics: &mut Vec<CanonicalPackageDiagnostic>) {
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
    match manifest
        .schema_version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u64>().ok())
    {
        Some(SUPPORTED_MAJOR) => {}
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

fn validate_unit(
    unit: &Unit,
    manifest: &Manifest,
    location: &str,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    for (field, value) in [
        ("source_profile", &unit.source_profile),
        ("work_id", &unit.work_id),
        ("version_id", &unit.version_id),
        ("language", &unit.language),
        ("text", &unit.text),
    ] {
        if value.trim().is_empty() {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_UNIT_REQUIRED_FIELD",
                location: location.into(),
                message: format!("{field} is required"),
            });
        }
    }
    if unit.components.is_empty() {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_UNIT_REQUIRED_FIELD",
            location: location.into(),
            message: "components is required".into(),
        });
    }
    for (field, actual, expected) in [
        ("source_profile", &unit.source_profile, &manifest.profile),
        ("work_id", &unit.work_id, &manifest.work_id),
        ("version_id", &unit.version_id, &manifest.version_id),
        ("language", &unit.language, &manifest.language),
    ] {
        if !actual.is_empty() && actual != expected {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH",
                location: location.into(),
                message: format!("{field} does not match manifest"),
            });
        }
    }
    if unit.backing_selectors.is_empty() {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_SELECTOR_MISSING",
            location: location.into(),
            message: "at least one backing selector is required".into(),
        });
    }
    if let Some(hash) = &unit.text_hash {
        if *hash != hex_sha256(unit.text.as_bytes()) {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_TEXT_HASH_MISMATCH",
                location: location.into(),
                message: "text_hash does not match text".into(),
            });
        }
    }
}

fn diagnostic(
    code: &'static str,
    location: &str,
    error: impl std::fmt::Display,
) -> CanonicalPackageDiagnostic {
    CanonicalPackageDiagnostic {
        code,
        location: location.into(),
        message: error.to_string(),
    }
}
