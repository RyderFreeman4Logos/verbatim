use std::io::Write;
use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use crate::client::CliError;

#[derive(Debug, Subcommand)]
pub(super) enum CanonicalCommand {
    Validate {
        #[arg(value_name = "PACKAGE")]
        package: PathBuf,
        #[arg(long, value_enum)]
        format: Option<CanonicalFormat>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum CanonicalFormat {
    Human,
    Json,
}

pub(super) fn run<W: Write>(command: CanonicalCommand, stdout: &mut W) -> Result<u8, CliError> {
    match command {
        CanonicalCommand::Validate { package, format } => {
            let report = verbatim_core::parser::canonical_package::validate_package(&package);
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
                    writeln!(
                        stdout,
                        "schema_version: {}",
                        report.schema_version.as_deref().unwrap_or("unknown")
                    )
                    .map_err(CliError::Io)?;
                    writeln!(stdout, "unit_count: {}", report.unit_count).map_err(CliError::Io)?;
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
    }
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
}
