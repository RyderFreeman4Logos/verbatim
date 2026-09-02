use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::parser::canonical_jsonl::{evidence_kind_from_content_kind, CanonicalJsonlParser};
use crate::profiles::bible::canon_registry::{CanonRegistry, VERSION as CANON_VERSION};
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, DerivedConversionMetadata, EvidenceId,
    EvidenceUnit, SourceId, SourceLocator,
};

const MANIFEST: &str = "manifest.json";
const UNITS: &str = "units.jsonl";
const RELATIONS: &str = "relations.jsonl";
const SUPPORTED_MAJOR: u64 = 1;
const USFM_SOURCE_NATIVE_SCHEME: &str = "usfm";

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
    pub package_hash: Option<String>,
    pub original_source_hash: Option<String>,
    pub conversion: Option<DerivedConversionMetadata>,
    pub units: Vec<CanonicalPackageUnitReport>,
    pub diagnostics: Vec<CanonicalPackageDiagnostic>,
    pub report_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalPackageUnitReport {
    pub locator: CanonicalLocator,
    pub original_source_hash: Option<String>,
    pub conversion: Option<DerivedConversionMetadata>,
    pub text_hash: String,
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
    canon_id: Option<String>,
    #[serde(default)]
    versification_id: Option<String>,
    #[serde(default)]
    language: String,
    #[serde(default)]
    original_source_hash: Option<String>,
    #[serde(default)]
    conversion: Option<DerivedConversionMetadata>,
}

#[derive(Debug, Default, Deserialize)]
struct Unit {
    #[serde(default)]
    unit_id: String,
    #[serde(default)]
    source_profile: String,
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    canon_id: Option<String>,
    #[serde(default)]
    versification_id: Option<String>,
    #[serde(default)]
    language: String,
    #[serde(default)]
    components: Vec<serde_json::Value>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    content_kind: String,
    #[serde(default)]
    text_hash: Option<String>,
    #[serde(default)]
    backing_selectors: Vec<BackingSelector>,
}

#[derive(Debug, Deserialize)]
struct PackageRelation {
    relation_type: String,
    from_unit_id: String,
    to_unit_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalPackageRelation {
    pub from_unit_id: EvidenceId,
    pub to_unit_id: EvidenceId,
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
        let manifest = read_manifest(path)?;
        let (canon_id, versification_id) = registry_ids(&manifest);
        let unit_ids = package_unit_ids(&path.join(UNITS))?;
        if units.len() != unit_ids.len() {
            bail!("canonical package unit identity count mismatch");
        }
        let source_id = SourceId::from_path(path);
        for (unit, unit_id) in units.iter_mut().zip(unit_ids) {
            unit.id = EvidenceId(unit_id);
            unit.source_id = source_id.clone();
            if let SourceLocator::Canonical { locator } = &mut unit.locator {
                locator.canon_id = Some(canon_id.to_string());
                locator.versification_id = Some(versification_id.to_string());
            }
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
    let mut unit_id_locations = HashMap::new();
    let mut unit_content_kinds = HashMap::new();
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
                    Ok(unit) => {
                        validate_unit(&unit, &manifest, &location, &mut diagnostics);
                        validate_unit_id(
                            &unit,
                            &location,
                            &mut unit_id_locations,
                            &mut diagnostics,
                        );
                        unit_content_kinds.insert(unit.unit_id.clone(), unit.content_kind.clone());
                    }
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
    validate_relation_endpoints(path, &unit_content_kinds, &mut diagnostics);
    let units = match CanonicalJsonlParser.parse(&units_path) {
        Ok(units) => {
            let (canon_id, versification_id) = registry_ids(&manifest);
            units
                .into_iter()
                .filter_map(|unit| match unit.locator {
                    SourceLocator::Canonical { mut locator } => {
                        locator.canon_id =
                            Some(locator.canon_id.unwrap_or_else(|| canon_id.to_string()));
                        locator.versification_id = Some(
                            locator
                                .versification_id
                                .unwrap_or_else(|| versification_id.to_string()),
                        );
                        Some(CanonicalPackageUnitReport {
                            locator,
                            original_source_hash: manifest.original_source_hash.clone(),
                            conversion: manifest.conversion.clone(),
                            text_hash: unit.text_hash,
                        })
                    }
                    _ => None,
                })
                .collect()
        }
        Err(error) => {
            diagnostics.push(diagnostic("CANONICAL_PACKAGE_UNIT_INVALID", UNITS, error));
            Vec::new()
        }
    };
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
    let package_hash = package_hash(path).ok();
    let payload = serde_json::json!({"valid": valid, "schema_version": manifest.schema_version, "unit_count": unit_count, "package_hash": package_hash, "original_source_hash": manifest.original_source_hash, "conversion": manifest.conversion, "units": units, "diagnostics": diagnostics});
    let report_hash = hex_sha256(
        serde_json::to_string(&payload)
            .expect("report serializes")
            .as_bytes(),
    );
    CanonicalPackageReport {
        valid,
        schema_version: (!manifest.schema_version.is_empty()).then_some(manifest.schema_version),
        unit_count,
        package_hash,
        original_source_hash: manifest.original_source_hash,
        conversion: manifest.conversion,
        units,
        diagnostics,
        report_hash,
    }
}

pub fn package_hash(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut names = vec![MANIFEST, UNITS];
    if path.join(RELATIONS).is_file() {
        names.push(RELATIONS);
    }
    for name in names {
        let bytes =
            fs::read(path.join(name)).with_context(|| format!("read package file {name}"))?;
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_relation_endpoints(
    path: &Path,
    unit_content_kinds: &HashMap<String, String>,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    let relations_path = path.join(RELATIONS);
    if !relations_path.is_file() {
        return;
    }
    let file = match fs::File::open(&relations_path) {
        Ok(file) => file,
        Err(error) => {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_RELATION_INVALID",
                RELATIONS,
                error,
            ));
            return;
        }
    };

    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line_no = index + 1;
        let location = format!("{RELATIONS}:{line_no}");
        let line = match line {
            Ok(line) if !line.trim().is_empty() => line,
            Ok(_) => continue,
            Err(error) => {
                diagnostics.push(diagnostic(
                    "CANONICAL_PACKAGE_RELATION_INVALID",
                    &location,
                    error,
                ));
                continue;
            }
        };
        let relation = match serde_json::from_str::<PackageRelation>(&line) {
            Ok(relation) => relation,
            Err(error) => {
                diagnostics.push(diagnostic(
                    "CANONICAL_PACKAGE_RELATION_INVALID",
                    &location,
                    error,
                ));
                continue;
            }
        };
        if relation.relation_type != "footnote_references_verse"
            || relation.from_unit_id.is_empty()
            || relation.to_unit_id.is_empty()
        {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_RELATION_INVALID",
                location,
                message: "relation type and endpoints are required".into(),
            });
            continue;
        }
        let (Some(from_kind), Some(to_kind)) = (
            unit_content_kinds.get(&relation.from_unit_id),
            unit_content_kinds.get(&relation.to_unit_id),
        ) else {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_RELATION_ENDPOINT_UNKNOWN",
                location,
                message: "relation endpoint does not name a package unit".into(),
            });
            continue;
        };
        if from_kind != "footnote" || to_kind != "verse" {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_RELATION_KIND_INVALID",
                location,
                message: "footnote_references_verse requires a footnote source and verse target"
                    .into(),
            });
        }
    }
}

pub(crate) fn package_relations(path: &Path) -> Result<Vec<CanonicalPackageRelation>> {
    let relations_path = path.join(RELATIONS);
    if !relations_path.is_file() {
        return Ok(Vec::new());
    }

    let mut relations = Vec::new();
    for (index, line) in BufReader::new(fs::File::open(&relations_path)?)
        .lines()
        .enumerate()
    {
        let line_no = index + 1;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let relation: PackageRelation = serde_json::from_str(&line)
            .with_context(|| format!("invalid relation on line {line_no}"))?;
        if relation.relation_type != "footnote_references_verse"
            || relation.from_unit_id.is_empty()
            || relation.to_unit_id.is_empty()
        {
            bail!("invalid relation on line {line_no}");
        }
        relations.push(CanonicalPackageRelation {
            from_unit_id: EvidenceId(relation.from_unit_id),
            to_unit_id: EvidenceId(relation.to_unit_id),
        });
    }
    Ok(relations)
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let contents = fs::read_to_string(path.join(MANIFEST))
        .with_context(|| format!("read {}", path.join(MANIFEST).display()))?;
    serde_json::from_str(&contents).context("parse canonical package manifest")
}

fn registry_ids(manifest: &Manifest) -> (&str, &str) {
    (
        manifest.canon_id.as_deref().unwrap_or(CANON_VERSION),
        manifest
            .versification_id
            .as_deref()
            .unwrap_or(VERSIFICATION_VERSION),
    )
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

fn validate_unit(
    unit: &Unit,
    manifest: &Manifest,
    location: &str,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    for (field, value) in [
        ("unit_id", &unit.unit_id),
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
    if evidence_kind_from_content_kind(&unit.content_kind).is_err() {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_UNIT_CONTENT_KIND_UNKNOWN",
            location: location.into(),
            message: format!("unknown content_kind {}", unit.content_kind),
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
    if let Some(actual) = &unit.canon_id {
        if actual != registry_ids(manifest).0 {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH",
                location: location.into(),
                message: "canon_id does not match manifest".into(),
            });
        }
    }
    if let Some(actual) = &unit.versification_id {
        if actual != registry_ids(manifest).1 {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH",
                location: location.into(),
                message: "versification_id does not match manifest".into(),
            });
        }
    }
    validate_reference_bounds(unit, manifest, location, diagnostics);
    if unit.backing_selectors.is_empty() {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_SELECTOR_MISSING",
            location: location.into(),
            message: "at least one backing selector is required".into(),
        });
    }
    for selector in &unit.backing_selectors {
        validate_source_native_selector(selector, unit, location, diagnostics);
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

fn validate_reference_bounds(
    unit: &Unit,
    manifest: &Manifest,
    location: &str,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    if unit.source_profile != "bible" {
        return;
    }
    let component = |level: &str| {
        unit.components
            .iter()
            .find(|component| {
                component.get("level").and_then(serde_json::Value::as_str) == Some(level)
            })
            .and_then(|component| component.get("value"))
            .and_then(serde_json::Value::as_str)
    };
    let Some(book) = component("book") else {
        return;
    };
    let Some(chapter_value) = component("chapter") else {
        return;
    };
    let Ok(chapter) = chapter_value.parse::<u16>() else {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS",
            location: location.into(),
            message: format!("reference chapter {chapter_value} is not a valid unsigned integer"),
        });
        return;
    };
    let Some(verse_value) = component("verse") else {
        return;
    };
    let Ok(verse) = verse_value.parse::<u16>() else {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS",
            location: location.into(),
            message: format!("reference verse {verse_value} is not a valid unsigned integer"),
        });
        return;
    };
    let Some(book) = CanonRegistry::resolve(book) else {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS",
            location: location.into(),
            message: "reference book is not in the selected canon".into(),
        });
        return;
    };
    let (_, versification_id) = registry_ids(manifest);
    if VersificationRegistry::by_id(versification_id).is_some()
        && VersificationRegistry::lookup(book.id, chapter, verse).is_none()
    {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS",
            location: location.into(),
            message: format!(
                "reference {} {}:{} is outside versification bounds",
                book.id, chapter, verse
            ),
        });
    }
}

fn validate_unit_id(
    unit: &Unit,
    location: &str,
    unit_id_locations: &mut HashMap<String, String>,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    if unit.unit_id.trim().is_empty() {
        return;
    }
    if let Some(first_location) = unit_id_locations.insert(unit.unit_id.clone(), location.into()) {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_UNIT_ID_DUPLICATE",
            location: location.into(),
            message: format!("unit_id duplicates {first_location}"),
        });
    }
}

fn package_unit_ids(path: &Path) -> Result<Vec<String>> {
    let mut unit_ids = Vec::new();
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        unit_ids.push(
            serde_json::from_str::<Unit>(&line)
                .map(|unit| unit.unit_id)
                .context("parse canonical package unit identity")?,
        );
    }
    Ok(unit_ids)
}

fn validate_source_native_selector(
    selector: &BackingSelector,
    unit: &Unit,
    location: &str,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    let BackingSelector::SourceNative { scheme, value } = selector else {
        return;
    };
    let expected = match source_native_selector_value(scheme, unit) {
        Ok(expected) => expected,
        Err(message) => {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_SOURCE_NATIVE_SELECTOR_INVALID",
                location: location.into(),
                message,
            });
            return;
        }
    };
    if value != &expected {
        diagnostics.push(CanonicalPackageDiagnostic {
            code: "CANONICAL_PACKAGE_SOURCE_NATIVE_SELECTOR_INVALID",
            location: location.into(),
            message: format!("{scheme} selector does not resolve to this canonical unit"),
        });
    }
}

fn source_native_selector_value(scheme: &str, unit: &Unit) -> Result<String, String> {
    if scheme != USFM_SOURCE_NATIVE_SCHEME {
        return Err(format!(
            "unsupported source-native selector scheme {scheme}"
        ));
    }
    let component = |level| {
        unit.components
            .iter()
            .find(|component| {
                component.get("level").and_then(serde_json::Value::as_str) == Some(level)
            })
            .and_then(|component| component.get("value"))
            .and_then(serde_json::Value::as_str)
    };
    let Some(book) = component("book") else {
        return Err("usfm selector requires a book component".into());
    };
    let Some(chapter) = component("chapter") else {
        return Err("usfm selector requires a chapter component".into());
    };
    let Some(verse) = component("verse") else {
        return Err("usfm selector requires a verse component".into());
    };
    let book = match book {
        "John" => "JHN",
        _ => return Err(format!("usfm selector cannot resolve book {book}")),
    };
    Ok(format!("{book} {chapter}:{verse}"))
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
