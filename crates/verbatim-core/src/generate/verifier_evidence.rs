use crate::types::report_artifact::is_report_artifact_id;
use crate::types::{CitationRef, EvidenceId, EvidenceKind, SourceLocator};

use super::{
    citation_kind_label, SourcePackAttachment, VerifierDerivationStep, VerifierEvidenceInput,
    VerifierEvidenceProvenance, VerifierLocatorInput, VerifierVisualSupport,
};

pub(super) fn verifier_source_inputs(
    citations: &[CitationRef],
    attachments: &[SourcePackAttachment],
) -> Vec<VerifierEvidenceInput> {
    citations
        .iter()
        .filter_map(|citation| {
            let evidence_id = citation.backing_evidence_id.as_ref().or_else(|| {
                (!is_report_artifact_id(&citation.evidence_id.0)).then_some(&citation.evidence_id)
            })?;
            Some(VerifierEvidenceInput {
                id: citation.label.clone(),
                evidence_id: evidence_id.0.clone(),
                source_id: citation.source_id.0.clone(),
                kind: citation_kind_label(citation.kind, citation.derived_from.as_ref()),
                locator: verifier_locator(&citation.locator),
                text: citation.text_preview.clone(),
                provenance: verifier_provenance(citation, evidence_id),
                visual_support: verifier_visual_support(citation, attachments),
            })
        })
        .collect()
}

fn verifier_provenance(
    citation: &CitationRef,
    evidence_id: &EvidenceId,
) -> VerifierEvidenceProvenance {
    let summary = match (citation.kind, citation.derived_from.as_ref()) {
        (EvidenceKind::Text, _) => "original source text",
        (EvidenceKind::Ocr, _) => "OCR-derived source text with structured locator",
        (EvidenceKind::Image, _) => "original image artifact locator",
        (EvidenceKind::Generated, Some(_)) => {
            "generated image caption derived from original image artifact"
        }
        (EvidenceKind::Generated, None) => "generated derived evidence",
    };

    VerifierEvidenceProvenance {
        summary,
        derived_from: citation.derived_from.as_ref().map(|id| id.0.clone()),
        derivation_chain: verifier_derivation_chain(citation, evidence_id),
    }
}

fn verifier_derivation_chain(
    citation: &CitationRef,
    evidence_id: &EvidenceId,
) -> Vec<VerifierDerivationStep> {
    match (citation.kind, citation.derived_from.as_ref()) {
        (EvidenceKind::Generated, Some(source_image_id)) => vec![
            VerifierDerivationStep {
                evidence_id: source_image_id.0.clone(),
                source_id: citation.source_id.0.clone(),
                kind: "image_artifact",
                locator: verifier_locator(&citation.locator),
                relation: "original_image_artifact",
            },
            VerifierDerivationStep {
                evidence_id: evidence_id.0.clone(),
                source_id: citation.source_id.0.clone(),
                kind: "image_caption_generated",
                locator: verifier_locator(&citation.locator),
                relation: "generated_caption_from_image",
            },
        ],
        _ => vec![VerifierDerivationStep {
            evidence_id: evidence_id.0.clone(),
            source_id: citation.source_id.0.clone(),
            kind: citation_kind_label(citation.kind, citation.derived_from.as_ref()),
            locator: verifier_locator(&citation.locator),
            relation: "source_evidence",
        }],
    }
}

fn verifier_visual_support(
    citation: &CitationRef,
    attachments: &[SourcePackAttachment],
) -> VerifierVisualSupport {
    let image_evidence_id = citation_image_artifact_evidence_id(citation).cloned();
    let vision_attachment = match &image_evidence_id {
        Some(_) if citation_has_attachment(citation, attachments) => "included",
        Some(_) => "not_included",
        None => "not_applicable",
    };

    let (support_level, caution) = match (citation.kind, citation.derived_from.as_ref()) {
        (EvidenceKind::Text, _) => (
            "text_only",
            "Use this source for document text claims, not visual content claims.",
        ),
        (EvidenceKind::Ocr, _) => (
            "ocr_text",
            "OCR-derived text can support document text claims; consider OCR confidence for weak scans.",
        ),
        (EvidenceKind::Image, _) if vision_attachment == "included" => (
            "image_pixels_available",
            "The verifier can inspect the cited image payload for visual claims.",
        ),
        (EvidenceKind::Image, _) => (
            "artifact_locator_only",
            "Image metadata identifies the artifact and location, but does not prove visual content without caption or pixels.",
        ),
        (EvidenceKind::Generated, Some(_)) if vision_attachment == "included" => (
            "caption_plus_pixels",
            "Generated caption is derived evidence; pixels are also available for visual verification.",
        ),
        (EvidenceKind::Generated, Some(_)) => (
            "caption_only_conservative",
            "Generated caption is weaker than original text or inspected pixels; revise over-strong visual claims.",
        ),
        (EvidenceKind::Generated, None) => (
            "generated_text",
            "Generated evidence is not original source text.",
        ),
    };

    VerifierVisualSupport {
        support_level,
        vision_attachment,
        image_evidence_id: image_evidence_id.map(|id| id.0),
        caution,
    }
}

fn verifier_locator(locator: &SourceLocator) -> VerifierLocatorInput {
    VerifierLocatorInput {
        display: locator.to_string(),
        structured: locator.clone(),
    }
}

fn citation_has_attachment(citation: &CitationRef, attachments: &[SourcePackAttachment]) -> bool {
    attachments.iter().any(|attachment| {
        attachment
            .labels
            .iter()
            .any(|label| label == &citation.label)
    })
}

fn citation_image_artifact_evidence_id(citation: &CitationRef) -> Option<&EvidenceId> {
    match citation.kind {
        EvidenceKind::Image => Some(&citation.evidence_id),
        EvidenceKind::Generated => citation.derived_from.as_ref(),
        EvidenceKind::Text | EvidenceKind::Ocr => None,
    }
}
