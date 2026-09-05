//! Bible hierarchy, range and document-order validation; registry owns canonical ordinals.
use std::collections::HashSet;

use super::{diagnostic, CanonicalPackageDiagnostic, Unit};
use crate::profiles::bible::canon_registry::CanonRegistry;
use crate::profiles::bible::versification_registry::VersificationRegistry;
use crate::types::{BackingSelector, ReferenceComponent};

#[derive(Default)]
pub(super) struct Ordering {
    previous: Option<Vec<u32>>,
    verse_end: Option<Vec<u32>>,
    identities: HashSet<Vec<u32>>,
}

impl Ordering {
    pub(super) fn validate(
        &mut self,
        unit: &Unit,
        location: &str,
        diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
    ) {
        validate_annotations(unit, location, diagnostics);
        let start = match coordinates(&unit.components, unit.content_kind == "footnote") {
            Ok(start) => start,
            Err(code) => {
                diagnostics.push(diagnostic(code, location, "expected ordered book/chapter/verse components with matching positive ordinals; footnotes append a note ordinal"));
                return;
            }
        };
        let end = match &unit.end_components {
            None => start.clone(),
            Some(components) => match coordinates(components, false) {
                Ok(end) if unit.content_kind != "footnote" && end >= start => end,
                _ => {
                    diagnostics.push(diagnostic(
                        "CANONICAL_PACKAGE_RANGE_INVALID",
                        location,
                        "range endpoint must be a complete bounded reference at or after start",
                    ));
                    return;
                }
            },
        };
        if !self.identities.insert(start.clone()) {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_LOCATOR_DUPLICATE",
                location,
                "duplicate canonical coordinate, including aliases",
            ));
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous > &start)
        {
            diagnostics.push(diagnostic(
                "CANONICAL_PACKAGE_ORDER_INVALID",
                location,
                "units must follow canonical order",
            ));
        }
        self.previous = Some(start.clone());
        if unit.content_kind != "footnote" {
            if self
                .verse_end
                .as_ref()
                .is_some_and(|previous| previous >= &start)
                && !diagnostics.iter().any(|d| {
                    d.location == location && d.code == "CANONICAL_PACKAGE_LOCATOR_DUPLICATE"
                })
            {
                diagnostics.push(diagnostic(
                    "CANONICAL_PACKAGE_RANGE_OVERLAP",
                    location,
                    "text unit overlaps a preceding canonical range",
                ));
            }
            self.verse_end = Some(end);
        }
    }
}

fn coordinates(values: &[serde_json::Value], note: bool) -> Result<Vec<u32>, &'static str> {
    let components: Vec<ReferenceComponent> = values
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|_| "CANONICAL_PACKAGE_HIERARCHY_INVALID")?;
    let expected = if note {
        &["book", "chapter", "verse", "note"][..]
    } else {
        &["book", "chapter", "verse"][..]
    };
    if components.len() != expected.len()
        || components
            .iter()
            .zip(expected)
            .any(|(component, level)| component.level != *level)
    {
        return Err("CANONICAL_PACKAGE_HIERARCHY_INVALID");
    }
    let mut ordinals = Vec::new();
    for (index, component) in components.iter().enumerate() {
        let expected = if index == 0 {
            u32::from(
                CanonRegistry::resolve(&component.value)
                    .ok_or("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS")?
                    .ordinal,
            )
        } else {
            let value: u32 = component
                .value
                .parse()
                .map_err(|_| "CANONICAL_PACKAGE_ORDINAL_INVALID")?;
            if value == 0 || value.to_string() != component.value {
                return Err("CANONICAL_PACKAGE_ORDINAL_INVALID");
            }
            value
        };
        if component.ordinal != Some(expected) {
            return Err("CANONICAL_PACKAGE_ORDINAL_INVALID");
        }
        ordinals.push(expected);
    }
    let book = components
        .first()
        .and_then(|c| CanonRegistry::resolve(&c.value))
        .ok_or("CANONICAL_PACKAGE_HIERARCHY_INVALID")?;
    let chapter = ordinals
        .get(1)
        .copied()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS")?;
    let verse = ordinals
        .get(2)
        .copied()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS")?;
    if VersificationRegistry::lookup(book.id, chapter, verse).is_none() {
        return Err("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS");
    }
    if !note {
        ordinals.push(0);
    }
    Ok(ordinals)
}

pub(super) fn valid_selector(selector: &BackingSelector) -> bool {
    match selector {
        BackingSelector::ByteRange { start, end } => start < end,
        BackingSelector::LineRange { start, end } => *start > 0 && start <= end,
        BackingSelector::TextQuote { exact, .. } => !exact.trim().is_empty(),
        BackingSelector::XmlId { id } => !id.is_empty() && !id.chars().any(char::is_whitespace),
        BackingSelector::SourceNative { scheme, value } => {
            !scheme.trim().is_empty() && !value.trim().is_empty()
        }
    }
}

fn validate_annotations(
    unit: &Unit,
    location: &str,
    diagnostics: &mut Vec<CanonicalPackageDiagnostic>,
) {
    let Some(annotations) = unit.metadata.get("annotations") else {
        return;
    };
    let valid = annotations.as_object().is_some_and(|map| {
        map.len() <= 8
            && map.iter().all(|(key, value)| {
                !key.is_empty() && key.len() <= 64 && value.as_str().is_some_and(|s| s.len() <= 64)
            })
    });
    if !valid {
        diagnostics.push(diagnostic("CANONICAL_PACKAGE_ANNOTATIONS_INVALID", location, "metadata.annotations must be an object of at most 8 string pairs, nonempty keys, maximum 64 UTF-8 bytes per key/value"));
    }
}
