//! Resolve command: parse a canonical reference and display its normalized form.

use std::io::Write;

use crate::client::CliError;
use crate::ResolveFormat;
use verbatim_core::profiles::{ProfileRegistry, ReferenceParseError};

pub(super) fn run_resolve<W>(
    reference: &str,
    format: Option<ResolveFormat>,
    stdout: &mut W,
) -> Result<(), CliError>
where
    W: Write,
{
    let registry = ProfileRegistry::new();
    let parsed = match registry.try_parse_with_diagnostic(reference) {
        Ok(parsed) => parsed,
        Err(ReferenceParseError::NotAReference) => {
            return Err(CliError::Api(format!(
                "could not parse \"{reference}\" as a canonical reference"
            )))
        }
        Err(error @ ReferenceParseError::OutOfBounds) => {
            return Err(CliError::Api(format!(
                "{}: reference \"{reference}\" is outside Protestant versification bounds",
                error
                    .diagnostic_code()
                    .unwrap_or("BIBLE_REFERENCE_OUT_OF_BOUNDS")
            )))
        }
    };

    // Build normalized key from start components
    let normalized: String = parsed
        .start
        .iter()
        .map(|c| c.value.to_lowercase().replace(' ', ""))
        .collect::<Vec<_>>()
        .join(":");

    match format {
        Some(ResolveFormat::Json) => {
            let end_display = parsed.end.as_ref().map(|end| {
                end.iter()
                    .map(|c| c.value.clone())
                    .collect::<Vec<_>>()
                    .join(":")
            });
            writeln!(
                stdout,
                "{}",
                serde_json::json!({
                    "profile": parsed.profile_id,
                    "raw": parsed.raw,
                    "display": parsed.display,
                    "normalized": normalized,
                    "start": parsed.start.iter().map(|c| serde_json::json!({
                        "level": c.level,
                        "value": c.value,
                        "ordinal": c.ordinal,
                    })).collect::<Vec<_>>(),
                    "end": end_display,
                })
            )?;
        }
        Some(ResolveFormat::Text) | None => {
            writeln!(stdout, "display:   {}", parsed.display)?;
            writeln!(stdout, "normalized: {}", normalized)?;
            writeln!(stdout, "profile:   {}", parsed.profile_id)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_reference_reports_stable_bounds_diagnostic() {
        for reference in ["John 3:999", "John 99:1"] {
            let mut stdout = Vec::new();
            let error = run_resolve(reference, None, &mut stdout).unwrap_err();
            assert!(stdout.is_empty(), "{reference}: {stdout:?}");
            assert!(error.to_string().contains("BIBLE_REFERENCE_OUT_OF_BOUNDS"));
        }
    }
}
