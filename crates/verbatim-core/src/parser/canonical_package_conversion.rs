use crate::parser::canonical_package::CanonicalPackageDiagnostic;
use crate::types::DerivedConversionMetadata;

pub(crate) fn validate_conversion(
    conversion: Option<&DerivedConversionMetadata>,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    let Some(conversion) = conversion else {
        return;
    };
    for (field, value) in [
        ("adapter", &conversion.adapter),
        ("converter", &conversion.converter),
        ("converter_version", &conversion.converter_version),
        ("original_source_hash", &conversion.original_source_hash),
        ("output_hash", &conversion.output_hash),
    ] {
        if value.trim().is_empty() {
            diagnostics.push(CanonicalPackageDiagnostic {
                code: "CANONICAL_PACKAGE_CONVERSION_INVALID",
                location: format!("manifest.json:conversion.{field}"),
                message: format!("conversion {field} is required"),
            });
        }
    }
}
