//! Lossless legacy record adapter. The legacy parser remains the evidence-ID authority.
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{files, validate_package, CanonicalPackageReport, MANIFEST, UNITS};
use crate::parser::canonical_jsonl::CanonicalJsonlParser;
use crate::profiles::bible::canon_registry::VERSION as CANON_VERSION;
use crate::profiles::bible::versification_registry::VERSION as VERSIFICATION_VERSION;
use crate::traits::Parser;
use crate::types::SourceLocator;

/// Adapt legacy JSONL to a new package, preserving raw input and existing evidence IDs.
///
/// `manifest` supplies identity/language and rights; missing work/edition identity is
/// taken from the first legacy record, never guessed. Other provenance is generated
/// from the input and running converter bytes. The absent destination is published
/// only after shared validation succeeds. The caller must own its destination path.
pub fn migrate_legacy_jsonl(
    input: &Path,
    manifest: &Path,
    destination: &Path,
) -> Result<CanonicalPackageReport> {
    if destination.try_exists()? {
        bail!("migration destination already exists");
    }
    let units = CanonicalJsonlParser.parse(input)?;
    let first = units.first().context("legacy JSONL must contain units")?;
    let SourceLocator::Canonical { locator } = &first.locator else {
        bail!("legacy input must be canonical");
    };
    let mut manifest: Value = serde_json::from_slice(&fs::read(manifest)?)?;
    let object = manifest
        .as_object_mut()
        .context("manifest must be an object")?;
    object
        .entry("work_id")
        .or_insert_with(|| json!(locator.work_id));
    object
        .entry("version_id")
        .or_insert_with(|| json!(locator.version_id));
    object.entry("language").or_insert_with(|| json!("und"));
    let language = object
        .get("language")
        .and_then(Value::as_str)
        .context("manifest language must be a string")?
        .to_owned();
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::Builder::new()
        .prefix(".verbatim-package-")
        .tempdir_in(parent)?;
    fs::create_dir(staged.path().join("source"))?;
    let bundled_source = staged.path().join("source/original.jsonl");
    std::io::copy(
        &mut fs::File::open(input)?,
        &mut fs::File::create(&bundled_source)?,
    )?;
    let source_hash = files::sha256_file(&bundled_source)?;

    let output_path = staged.path().join(UNITS);
    let mut output = BufWriter::new(fs::File::create(&output_path)?);
    let mut output_hasher = Sha256::new();
    let mut evidence = units.iter();
    for line in BufReader::new(fs::File::open(&bundled_source)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let evidence = evidence.next().context("legacy unit count changed")?;
        let mut record: Value = serde_json::from_str(&line)?;
        let fields = record
            .as_object_mut()
            .context("legacy unit must be an object")?;
        fields.insert("unit_id".into(), json!(evidence.id.0));
        fields.insert("text_hash".into(), json!(evidence.text_hash));
        let SourceLocator::Canonical { locator } = &evidence.locator else {
            bail!("legacy input must be canonical");
        };
        fields.insert(
            "backing_selectors".into(),
            serde_json::to_value(&locator.backing_selectors)?,
        );
        for field in ["work_id", "version_id"] {
            fields
                .entry(field)
                .or_insert_with(|| object.get(field).cloned().unwrap_or(Value::Null));
        }
        fields
            .entry("language")
            .or_insert_with(|| json!(if language == "mul" { "und" } else { &language }));
        let bytes = serde_json::to_vec(&record)?;
        output.write_all(&bytes)?;
        output.write_all(b"\n")?;
        output_hasher.update(&bytes);
        output_hasher.update(b"\n");
    }
    if evidence.next().is_some() {
        bail!("legacy unit count changed");
    }
    output.flush()?;
    drop(output);
    let output_hash = format!("{:x}", output_hasher.finalize());
    let converter_artifact_hash = files::sha256_file(&std::env::current_exe()?)?;
    object.insert("schema_version".into(), json!("1.0.0"));
    object.insert("profile".into(), json!(locator.profile_id));
    object.insert("content_kind".into(), json!("text"));
    object
        .entry("canon_id")
        .or_insert_with(|| json!(CANON_VERSION));
    object
        .entry("versification_id")
        .or_insert_with(|| json!(VERSIFICATION_VERSION));
    object.insert("original_source".into(), json!({"format":"canonical-jsonl", "name":input.file_name().and_then(|name| name.to_str()).context("input name must be UTF-8")?, "path":"source/original.jsonl"}));
    object.insert("original_source_hash".into(), json!(source_hash));
    object.insert(
        "converter_artifact_hash".into(),
        json!(converter_artifact_hash),
    );
    object.insert("conversion".into(), json!({"adapter":"legacy-canonical-jsonl", "converter":"verbatim.canonical.migrate", "converter_version":env!("CARGO_PKG_VERSION"), "original_source_hash":source_hash, "output_hash":output_hash}));
    object.insert("validation".into(), json!({"status":"passed", "validator":"verbatim.canonical.validate", "version":env!("CARGO_PKG_VERSION")}));
    object.insert("files".into(), json!([{"path":"source/original.jsonl", "sha256":source_hash, "media_type":"application/x-ndjson"}]));
    fs::write(
        staged.path().join(MANIFEST),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let report = validate_package(staged.path());
    if !report.valid {
        let diagnostic = report
            .diagnostics
            .first()
            .context("invalid migration report")?;
        bail!(
            "{} at {}: {}",
            diagnostic.code,
            diagnostic.location,
            diagnostic.message
        );
    }
    if destination.try_exists()? {
        bail!("migration destination already exists");
    }
    fs::rename(staged.path(), destination)?;
    Ok(report)
}
