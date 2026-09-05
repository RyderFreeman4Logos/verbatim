//! Package-relative inventory and byte-integrity checks. Never follow member symlinks.
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use super::{
    diagnostic, schema::sha256, CanonicalPackageDiagnostic, Manifest, MANIFEST, RELATIONS, UNITS,
};
use crate::types::hex_sha256;
use anyhow::{bail, Context, Result};

fn safe_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\\', ':'])
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

pub(super) fn names(root: &Path) -> Result<Vec<String>> {
    let mut directories = vec![root.to_path_buf()];
    let mut names = Vec::new();
    while let Some(directory) = directories.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("package names must be UTF-8")?
                .to_owned();
            let kind = entry.file_type()?;
            if !safe_path(&name) || kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                bail!("unsafe package member {name}");
            }
            if kind.is_dir() {
                if name != "assets"
                    && name != "source"
                    && !name.starts_with("assets/")
                    && !name.starts_with("source/")
                {
                    bail!("unknown package directory {name}");
                }
                directories.push(path);
            } else {
                if ![MANIFEST, UNITS, RELATIONS].contains(&name.as_str())
                    && !name.starts_with("assets/")
                    && !name.starts_with("source/")
                {
                    bail!("unknown package file {name}");
                }
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

pub(super) fn validate(
    root: &Path,
    names: &[String],
    manifest: &Manifest,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    let available: HashSet<_> = names.iter().map(String::as_str).collect();
    let mut declared = HashSet::new();
    for (index, file) in manifest.files.iter().enumerate() {
        let location = format!("manifest.json:files.{index}.path");
        if !safe_path(&file.path)
            || (!file.path.starts_with("assets/") && !file.path.starts_with("source/"))
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_PATH_INVALID",
                &location,
                "files must name portable relative assets/ or source/ paths",
            ));
            continue;
        }
        if !declared.insert(file.path.as_str()) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_DUPLICATE",
                &location,
                "duplicate inventory path",
            ));
        }
        if !sha256(&file.sha256)
            || !file.media_type.split_once('/').is_some_and(|(a, b)| {
                !a.is_empty() && !b.is_empty() && !file.media_type.chars().any(char::is_whitespace)
            })
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_METADATA_INVALID",
                &format!("manifest.json:files.{index}"),
                "lowercase SHA-256 and media type are required",
            ));
        }
        if !available.contains(file.path.as_str()) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_MISSING",
                &file.path,
                "declared file is missing",
            ));
            continue;
        }
        match fs::read(root.join(&file.path)) {
            Ok(bytes) if hex_sha256(&bytes) == file.sha256 => {}
            Ok(_) => diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_HASH_MISMATCH",
                &file.path,
                "file bytes do not match declared SHA-256",
            )),
            Err(error) => diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_READ_FAILED",
                &file.path,
                error,
            )),
        }
    }
    for name in names {
        if (name.starts_with("assets/") || name.starts_with("source/"))
            && !declared.contains(name.as_str())
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_UNDECLARED",
                name,
                "every asset/source file must be inventoried",
            ));
        }
    }
    if let Some(source) = manifest
        .original_source
        .as_ref()
        .and_then(|s| s.path.as_ref())
    {
        if !safe_path(source) || !source.starts_with("source/") {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_FILE_PATH_INVALID",
                "manifest.json:original_source.path",
                "bundled original must be below source/",
            ));
        } else if !manifest.files.iter().any(|file| {
            &file.path == source && Some(&file.sha256) == manifest.original_source_hash.as_ref()
        }) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_SOURCE_HASH_MISMATCH",
                source,
                "bundled original must be inventoried with original_source_hash",
            ));
        }
    }
    if let (Some(conversion), Ok(bytes)) = (&manifest.conversion, fs::read(root.join(UNITS))) {
        if conversion.output_hash != hex_sha256(&bytes) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_OUTPUT_HASH_MISMATCH",
                UNITS,
                "conversion.output_hash must match exact units.jsonl bytes",
            ));
        }
    }
}
