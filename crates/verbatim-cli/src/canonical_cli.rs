use std::io::Write;
use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use crate::client::CliError;

#[derive(Debug, Subcommand)]
pub(super) enum CanonicalCommand {
    Inspect {
        #[arg(value_name = "EPUB")]
        epub: PathBuf,
        #[arg(long, value_enum)]
        format: Option<CanonicalFormat>,
    },
    /// Convert legacy JSONL into a new, validated package; preserve the original bytes.
    Migrate {
        input: PathBuf,
        destination: PathBuf,
        #[arg(long, value_name = "IDENTITY_JSON")]
        manifest: PathBuf,
        #[arg(long, value_enum)]
        format: Option<CanonicalFormat>,
    },
    /// Validate a package locally before ingestion, including all inventoried files.
    Validate {
        #[arg(value_name = "PACKAGE")]
        package: PathBuf,
        #[arg(long, value_enum)]
        format: Option<CanonicalFormat>,
        /// Return nonzero for warnings as well as errors.
        #[arg(long)]
        deny_warnings: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum CanonicalFormat {
    Human,
    Json,
}

pub(super) fn run<W: Write>(command: CanonicalCommand, stdout: &mut W) -> Result<u8, CliError> {
    match command {
        CanonicalCommand::Inspect { epub, format } => {
            let report = verbatim_core::parser::epub_inspect::inspect_epub(&epub);
            match format.unwrap_or(CanonicalFormat::Human) {
                CanonicalFormat::Json => writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(&report)
                        .map_err(|error| CliError::Api(error.to_string()))?
                )
                .map_err(CliError::Io)?,
                CanonicalFormat::Human => {
                    writeln!(stdout, "valid: {}", report.valid).map_err(CliError::Io)?;
                    writeln!(stdout, "entry_count: {}", report.entry_count)
                        .map_err(CliError::Io)?;
                    for diagnostic in &report.diagnostics {
                        writeln!(
                            stdout,
                            "{} {}: {}",
                            diagnostic.code, diagnostic.location, diagnostic.message
                        )
                        .map_err(CliError::Io)?;
                    }
                    writeln!(stdout, "report_hash: {}", report.report_hash)
                        .map_err(CliError::Io)?;
                }
            }
            Ok(if report.valid { 0 } else { 1 })
        }
        CanonicalCommand::Migrate {
            input,
            destination,
            manifest,
            format,
        } => {
            let report = verbatim_core::parser::canonical_package::migrate_legacy_jsonl(
                &input,
                &manifest,
                &destination,
            )
            .map_err(|error| CliError::Api(error.to_string()))?;
            write_package_report(&report, format, false, stdout)
        }
        CanonicalCommand::Validate {
            package,
            format,
            deny_warnings,
        } => {
            let report = verbatim_core::parser::canonical_package::validate_package(&package);
            write_package_report(&report, format, deny_warnings, stdout)
        }
    }
}

fn write_package_report<W: Write>(
    report: &verbatim_core::parser::canonical_package::CanonicalPackageReport,
    format: Option<CanonicalFormat>,
    deny_warnings: bool,
    stdout: &mut W,
) -> Result<u8, CliError> {
    match format.unwrap_or(CanonicalFormat::Human) {
        CanonicalFormat::Json => writeln!(
            stdout,
            "{}",
            serde_json::to_string(&report).map_err(|error| CliError::Api(error.to_string()))?
        )
        .map_err(CliError::Io)?,
        CanonicalFormat::Human => {
            writeln!(stdout, "valid: {}", report.valid).map_err(CliError::Io)?;
            writeln!(
                stdout,
                "schema_version: {}",
                report.schema_version.as_deref().unwrap_or("unknown")
            )
            .map_err(CliError::Io)?;
            writeln!(stdout, "unit_count: {}", report.unit_count).map_err(CliError::Io)?;
            for diagnostic in report.diagnostics.iter().chain(&report.warnings) {
                writeln!(
                    stdout,
                    "{} {}: {}",
                    diagnostic.code, diagnostic.location, diagnostic.message
                )
                .map_err(CliError::Io)?;
            }
            writeln!(stdout, "report_hash: {}", report.report_hash).map_err(CliError::Io)?;
        }
    }
    Ok(
        if report.valid && (!deny_warnings || report.warnings.is_empty()) {
            0
        } else {
            1
        },
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn canonical_validate_cli_human_and_json() {
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../verbatim-core/tests/fixtures/canonical_package/valid");
        let mut output = Vec::new();

        assert_eq!(
            run(
                CanonicalCommand::Validate {
                    package: package.clone(),
                    format: None,
                    deny_warnings: false,
                },
                &mut output,
            )
            .unwrap(),
            0
        );
        assert!(String::from_utf8_lossy(&output).contains("valid: true"));

        output.clear();
        assert_eq!(
            run(
                CanonicalCommand::Validate {
                    package,
                    format: Some(CanonicalFormat::Json),
                    deny_warnings: false,
                },
                &mut output,
            )
            .unwrap(),
            0
        );
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["unit_count"], 2);
        assert_eq!(report["report_hash"].as_str().unwrap().len(), 64);
        assert_eq!(
            report["original_source_hash"],
            "0bc1dd60d3bb6082799548fd022a62108d510b59523e04de6071e396b79d018c"
        );
        assert_eq!(report["conversion"]["converter"], "fixture-converter");
        assert_eq!(
            report["units"][0]["locator"]["backing_selectors"][0]["type"],
            "SourceNative"
        );
    }

    #[test]
    fn canonical_validate_cli_warning_policy_and_migration_command() {
        use clap::FromArgMatches;
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../verbatim-core/tests/fixtures/canonical_package/valid");
        let matches = CanonicalCommand::augment_subcommands(clap::Command::new("canonical"))
            .try_get_matches_from([
                "canonical",
                "validate",
                package.to_str().unwrap(),
                "--deny-warnings",
                "--format",
                "json",
            ])
            .expect("warning policy must be exposed");
        let command = CanonicalCommand::from_arg_matches(&matches).unwrap();
        let mut output = Vec::new();
        assert_eq!(run(command, &mut output).unwrap(), 1);
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["valid"], true);
        assert_eq!(
            report["warnings"][0]["code"],
            "CANONICAL_PACKAGE_SOURCE_NOT_BUNDLED"
        );
    }

    #[test]
    fn canonical_package_cli_migration_is_exposed() {
        assert!(
            CanonicalCommand::augment_subcommands(clap::Command::new("canonical"))
                .try_get_matches_from([
                    "canonical",
                    "migrate",
                    "legacy.jsonl",
                    "new-package",
                    "--manifest",
                    "identity.json"
                ])
                .is_ok()
        );
    }

    #[test]
    fn canonical_validate_cli_errors_have_locations_and_nonzero_exit() {
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../verbatim-core/tests/fixtures/canonical_package/invalid-text-hash");
        let mut output = Vec::new();
        assert_eq!(
            run(
                CanonicalCommand::Validate {
                    package,
                    format: None,
                    deny_warnings: false
                },
                &mut output
            )
            .unwrap(),
            1
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("CANONICAL_PACKAGE_TEXT_HASH_MISMATCH units.jsonl:1"));
        assert!(text.contains("report_hash:"));
    }

    #[test]
    fn canonical_inspect_cli_json_reports_epub_chain() {
        let epub = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../verbatim-core/tests/fixtures/epub/minimal.epub");
        let mut output = Vec::new();

        assert_eq!(
            run(
                CanonicalCommand::Inspect {
                    epub,
                    format: Some(CanonicalFormat::Json),
                },
                &mut output,
            )
            .unwrap(),
            0
        );
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["valid"], true);
        assert_eq!(report["container"]["path"], "META-INF/container.xml");
        assert_eq!(report["opf"]["path"], "OEBPS/content.opf");
        assert_eq!(report["spine"]["item_count"], 1);
        assert_eq!(report["navigation"]["path"], "OEBPS/nav.xhtml");
        assert_eq!(report["representative_spine_items"][0]["idref"], "chapter");
        assert_eq!(report["semantic_attributes"]["epub:type"]["toc"], 1);
        assert_eq!(report["class_distribution"]["chapter"], 1);
    }

    #[test]
    fn canonical_inspect_cli_missing_anchors_fail_closed() {
        for fixture in ["missing-container.epub", "missing-opf.epub"] {
            let epub = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../verbatim-core/tests/fixtures/epub")
                .join(fixture);
            let mut output = Vec::new();
            assert_eq!(
                run(
                    CanonicalCommand::Inspect {
                        epub,
                        format: Some(CanonicalFormat::Json),
                    },
                    &mut output,
                )
                .unwrap(),
                1,
                "{fixture} should fail closed"
            );
            let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(report["valid"], false);
            assert!(!report["diagnostics"].as_array().unwrap().is_empty());
            let code = report["diagnostics"][0]["code"].as_str().unwrap();
            assert!(
                (fixture == "missing-container.epub" && code == "EPUB_CONTAINER_MISSING")
                    || (fixture == "missing-opf.epub" && code == "EPUB_OPF_MISSING"),
                "unexpected diagnostic for {fixture}: {code}"
            );
        }
    }
}
