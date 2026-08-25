//! Resolve command: parse a canonical reference and display its normalized form.

use std::io::Write;

use crate::client::CliError;
use crate::ResolveFormat;

pub(super) fn run_resolve<W>(
    reference: &str,
    format: Option<ResolveFormat>,
    stdout: &mut W,
) -> Result<(), CliError>
where
    W: Write,
{
    let registry = verbatim_core::profiles::ProfileRegistry::new();
    let parsed = registry.try_parse(reference).ok_or_else(|| {
        CliError::Api(format!(
            "could not parse \"{reference}\" as a canonical reference"
        ))
    })?;

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
