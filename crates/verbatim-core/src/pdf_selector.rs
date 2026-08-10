//! Versioned, fail-closed resolution for born-digital PDF evidence selectors.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::types::{hex_sha256, EvidenceKind, EvidenceUnit, SourceLocator};

/// Current persisted PDF selector schema.
pub const PDF_SELECTOR_VERSION: u16 = 1;
const NORMALIZATION_PROFILE: &str = "unicode_whitespace_v1";
const MAX_QUOTE_BYTES: usize = 1_024;
const CONTEXT_CHARS: usize = 64;

/// Strength available from a PDF evidence locator before resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfAnchorStrength {
    /// Historical page and parser-local paragraph coordinates only.
    LegacyPageParagraph,
    /// A source-bound selector with normalized text anchors.
    VersionedSelector,
}

impl SourceLocator {
    /// Build the historical page/paragraph locator without stronger anchors.
    pub fn legacy_pdf(page: u32, paragraph: u32, bbox: Option<crate::types::BBox>) -> Self {
        Self::Pdf {
            page,
            paragraph,
            bbox,
            selector: None,
        }
    }

    /// Report the available anchor strength for ordinary PDF text locators.
    pub fn pdf_anchor_strength(&self) -> Option<PdfAnchorStrength> {
        match self {
            Self::Pdf { selector, .. } => Some(if selector.is_some() {
                PdfAnchorStrength::VersionedSelector
            } else {
                PdfAnchorStrength::LegacyPageParagraph
            }),
            _ => None,
        }
    }
}

/// Selector used to verify a known normalized page-text range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTextPosition {
    pub start: u32,
    pub end: u32,
}

/// Bounded exact quote plus optional surrounding context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTextQuote {
    pub exact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// Versioned selector persisted on ordinary PDF evidence locators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfSelector {
    pub version: u16,
    pub normalization_profile: String,
    pub source_hash: String,
    pub parser_profile_id: String,
    pub page_text_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<PdfTextPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<PdfTextQuote>,
    pub selected_text_hash: String,
}

/// Exact selector path that established a resolved range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfResolutionBasis {
    TextPosition,
    TextQuote,
}

/// Fail-closed result of resolving one PDF selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PdfResolutionOutcome {
    Exact {
        start: u32,
        end: u32,
        basis: PdfResolutionBasis,
    },
    Reanchored {
        start: u32,
        end: u32,
        basis: PdfResolutionBasis,
    },
    Ambiguous {
        matches: u32,
    },
    NotFound,
    SourceMismatch,
    Unsupported,
}

impl PdfSelector {
    /// Build a source-bound selector from a byte range in normalized page text.
    pub fn from_page_range(
        source_hash: String,
        parser_profile_id: &str,
        page_text: &str,
        range: Range<usize>,
    ) -> Option<Self> {
        let page_text = normalize_pdf_text(page_text);
        if range.start >= range.end
            || range.end > page_text.len()
            || !page_text.is_char_boundary(range.start)
            || !page_text.is_char_boundary(range.end)
        {
            return None;
        }
        let selected = &page_text[range.clone()];
        let start = u32::try_from(range.start).ok()?;
        let end = u32::try_from(range.end).ok()?;
        let quote = (selected.len() <= MAX_QUOTE_BYTES).then(|| PdfTextQuote {
            exact: selected.to_string(),
            prefix: bounded_suffix(&page_text[..range.start]),
            suffix: bounded_prefix(&page_text[range.end..]),
        });

        Some(Self {
            version: PDF_SELECTOR_VERSION,
            normalization_profile: NORMALIZATION_PROFILE.to_string(),
            source_hash,
            parser_profile_id: parser_profile_id.to_string(),
            page_text_hash: hex_sha256(page_text.as_bytes()),
            position: Some(PdfTextPosition { start, end }),
            quote,
            selected_text_hash: hex_sha256(selected.as_bytes()),
        })
    }

    /// Build a bounded quote-only selector when no position or context exists.
    pub fn quote_only(
        source_hash: String,
        parser_profile_id: &str,
        page_text: &str,
        exact: &str,
    ) -> Option<Self> {
        let page_text = normalize_pdf_text(page_text);
        let exact = normalize_pdf_text(exact);
        if exact.is_empty() || exact.len() > MAX_QUOTE_BYTES {
            return None;
        }
        Some(Self {
            version: PDF_SELECTOR_VERSION,
            normalization_profile: NORMALIZATION_PROFILE.to_string(),
            source_hash,
            parser_profile_id: parser_profile_id.to_string(),
            page_text_hash: hex_sha256(page_text.as_bytes()),
            position: None,
            selected_text_hash: hex_sha256(exact.as_bytes()),
            quote: Some(PdfTextQuote {
                exact,
                prefix: None,
                suffix: None,
            }),
        })
    }

    /// Resolve against the supplied source bytes and current page text.
    pub fn resolve(&self, source_bytes: &[u8], page_text: &str) -> PdfResolutionOutcome {
        if hex_sha256(source_bytes) != self.source_hash {
            return PdfResolutionOutcome::SourceMismatch;
        }
        if self.version != PDF_SELECTOR_VERSION
            || self.normalization_profile != NORMALIZATION_PROFILE
        {
            return PdfResolutionOutcome::Unsupported;
        }

        let page_text = normalize_pdf_text(page_text);
        let page_hash_matches = hex_sha256(page_text.as_bytes()) == self.page_text_hash;
        if page_hash_matches {
            if let Some(position) = self.position {
                let range = position.start as usize..position.end as usize;
                if page_text
                    .get(range)
                    .is_some_and(|text| hex_sha256(text.as_bytes()) == self.selected_text_hash)
                {
                    return PdfResolutionOutcome::Exact {
                        start: position.start,
                        end: position.end,
                        basis: PdfResolutionBasis::TextPosition,
                    };
                }
            }
        }

        let Some(quote) = &self.quote else {
            return PdfResolutionOutcome::NotFound;
        };
        if hex_sha256(quote.exact.as_bytes()) != self.selected_text_hash {
            return PdfResolutionOutcome::Unsupported;
        }

        let mut first = None;
        let mut matches = 0_u32;
        for (start, _) in page_text.match_indices(&quote.exact) {
            let end = start + quote.exact.len();
            if quote
                .prefix
                .as_ref()
                .is_some_and(|prefix| !page_text[..start].ends_with(prefix))
                || quote
                    .suffix
                    .as_ref()
                    .is_some_and(|suffix| !page_text[end..].starts_with(suffix))
            {
                continue;
            }
            matches = matches.saturating_add(1);
            first.get_or_insert((start, end));
        }

        match (matches, first) {
            (0, _) => PdfResolutionOutcome::NotFound,
            (1, Some((start, end))) => {
                let Ok(start) = u32::try_from(start) else {
                    return PdfResolutionOutcome::Unsupported;
                };
                let Ok(end) = u32::try_from(end) else {
                    return PdfResolutionOutcome::Unsupported;
                };
                if page_hash_matches {
                    PdfResolutionOutcome::Exact {
                        start,
                        end,
                        basis: PdfResolutionBasis::TextQuote,
                    }
                } else {
                    PdfResolutionOutcome::Reanchored {
                        start,
                        end,
                        basis: PdfResolutionBasis::TextQuote,
                    }
                }
            }
            _ => PdfResolutionOutcome::Ambiguous { matches },
        }
    }
}

/// Populate selectors on new born-digital PDF evidence before persistence.
pub(crate) fn attach_pdf_selectors(
    evidence: &mut [EvidenceUnit],
    source_hash: &str,
    parser_profile_id: &str,
) {
    let mut pages = BTreeMap::<u32, Vec<usize>>::new();
    for (index, unit) in evidence.iter().enumerate() {
        if unit.kind == EvidenceKind::Text {
            if let SourceLocator::Pdf {
                page,
                selector: None,
                ..
            } = &unit.locator
            {
                pages.entry(*page).or_default().push(index);
            }
        }
    }

    for indices in pages.values() {
        let mut page_text = String::new();
        let mut ranges = Vec::with_capacity(indices.len());
        for &index in indices {
            let selected = normalize_pdf_text(&evidence[index].text);
            if selected.is_empty() {
                continue;
            }
            if !page_text.is_empty() {
                page_text.push(' ');
            }
            let start = page_text.len();
            page_text.push_str(&selected);
            ranges.push((index, start..page_text.len()));
        }
        for (index, range) in ranges {
            let Some(selector) = PdfSelector::from_page_range(
                source_hash.to_string(),
                parser_profile_id,
                &page_text,
                range,
            ) else {
                continue;
            };
            if let SourceLocator::Pdf { selector: slot, .. } = &mut evidence[index].locator {
                *slot = Some(selector);
            }
        }
    }
}

fn normalize_pdf_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded_prefix(text: &str) -> Option<String> {
    let value = text.chars().take(CONTEXT_CHARS).collect::<String>();
    (!value.is_empty()).then_some(value)
}

fn bounded_suffix(text: &str) -> Option<String> {
    let mut chars = text.chars().rev().take(CONTEXT_CHARS).collect::<Vec<_>>();
    chars.reverse();
    let value = chars.into_iter().collect::<String>();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::{
        attach_pdf_selectors, PdfAnchorStrength, PdfResolutionBasis, PdfResolutionOutcome,
        PdfSelector,
    };
    use crate::types::{
        hex_sha256, EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator,
    };

    #[test]
    fn issue_288_source_hash_mismatch_fails_closed() {
        let selector = PdfSelector::from_page_range(
            hex_sha256(b"original-pdf"),
            "pdf_oxide",
            "alpha unique quote omega",
            6..18,
        )
        .unwrap();

        assert_eq!(
            selector.resolve(b"changed-pdf", "alpha unique quote omega"),
            PdfResolutionOutcome::SourceMismatch
        );
    }

    #[test]
    fn issue_288_unique_position_resolves_exactly() {
        let bytes = b"original-pdf";
        let selector = PdfSelector::from_page_range(
            hex_sha256(bytes),
            "pdf_oxide",
            "alpha unique quote omega",
            6..18,
        )
        .unwrap();

        assert_eq!(
            selector.resolve(bytes, "alpha unique quote omega"),
            PdfResolutionOutcome::Exact {
                start: 6,
                end: 18,
                basis: PdfResolutionBasis::TextPosition,
            }
        );
    }

    #[test]
    fn issue_288_duplicate_quote_without_disambiguators_is_ambiguous() {
        let bytes = b"original-pdf";
        let selector = PdfSelector::quote_only(
            hex_sha256(bytes),
            "pdf_oxide",
            "repeat once repeat",
            "repeat",
        )
        .unwrap();

        assert_eq!(
            selector.resolve(bytes, "repeat once repeat"),
            PdfResolutionOutcome::Ambiguous { matches: 2 }
        );
    }

    #[test]
    fn issue_288_ingest_attachment_populates_versioned_selector() {
        let mut evidence = vec![EvidenceUnit {
            id: EvidenceId("ev-1".into()),
            source_id: SourceId("source-1".into()),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Pdf {
                page: 1,
                paragraph: 0,
                bbox: None,
                selector: None,
            },
            text: "unique quote".into(),
            text_hash: hex_sha256(b"unique quote"),
            heading_path: Vec::new(),
            position: 0,
        }];

        attach_pdf_selectors(&mut evidence, &hex_sha256(b"original-pdf"), "pdf_oxide");

        let SourceLocator::Pdf { selector, .. } = &evidence[0].locator else {
            panic!("expected PDF locator");
        };
        assert_eq!(selector.as_ref().unwrap().version, 1);
        assert_eq!(
            evidence[0].locator.pdf_anchor_strength(),
            Some(PdfAnchorStrength::VersionedSelector)
        );
    }

    #[test]
    fn issue_288_legacy_locator_remains_readable_with_reduced_strength() {
        let locator: SourceLocator = serde_json::from_value(serde_json::json!({
            "type": "Pdf",
            "page": 4,
            "paragraph": 2,
            "bbox": null
        }))
        .unwrap();

        assert_eq!(
            locator.pdf_anchor_strength(),
            Some(PdfAnchorStrength::LegacyPageParagraph)
        );
        assert!(locator.to_string().contains("legacy anchor"));
    }
}
