use crate::types::report_artifact::ReportArtifactId;
use crate::types::{EvidenceUnit, RetrievalEvidenceRole, RetrievalOrigin, RetrievalProvenance};

pub(crate) fn evidence_debug_role(
    origin: RetrievalOrigin,
    evidence: &EvidenceUnit,
) -> RetrievalEvidenceRole {
    if origin == RetrievalOrigin::GraphReport {
        return RetrievalEvidenceRole::GraphReport;
    }

    match evidence.kind {
        crate::types::EvidenceKind::Text => RetrievalEvidenceRole::OriginalText,
        crate::types::EvidenceKind::Verse => RetrievalEvidenceRole::OriginalText,
        crate::types::EvidenceKind::Footnote => RetrievalEvidenceRole::OriginalText,
        crate::types::EvidenceKind::Ocr => RetrievalEvidenceRole::OcrText,
        crate::types::EvidenceKind::Image => RetrievalEvidenceRole::ImageArtifact,
        crate::types::EvidenceKind::Generated if evidence.derived_from.is_some() => {
            RetrievalEvidenceRole::ImageCaptionGenerated
        }
        crate::types::EvidenceKind::Generated => RetrievalEvidenceRole::Generated,
    }
}

pub(crate) fn graph_report_provenance(
    result_rank: usize,
    report_artifact_id: ReportArtifactId,
    generation: &str,
    content_hash: &str,
) -> RetrievalProvenance {
    RetrievalProvenance {
        origin: RetrievalOrigin::GraphReport,
        report_artifact_id: Some(report_artifact_id),
        report_artifact_schema_version: Some(crate::wire_schemas::WIRE_SCHEMA_VERSION),
        report_artifact_generation: Some(generation.to_owned()),
        report_artifact_content_hash: Some(content_hash.to_owned()),
        result_rank,
        seed_rank: None,
        seed_chunk_id: None,
        seed_source_id: None,
        hop_distance: 0,
        graph_path: Vec::new(),
    }
}
