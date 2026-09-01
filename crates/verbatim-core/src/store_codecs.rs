use rusqlite::types::Type;
use serde::de::DeserializeOwned;

use crate::types::{ChunkType, EdgeType, EvidenceKind, GraphNodeKind, SourceStatus};
use crate::vision_caption::ImageCaptionStatus;

pub(super) fn status_to_str(status: &SourceStatus) -> &'static str {
    match status {
        SourceStatus::Pending => "Pending",
        SourceStatus::Indexed => "Indexed",
        SourceStatus::Stale => "Stale",
    }
}

pub(super) fn str_to_status(status: &str) -> SourceStatus {
    match status {
        "Indexed" => SourceStatus::Indexed,
        "Stale" => SourceStatus::Stale,
        _ => SourceStatus::Pending,
    }
}

pub(super) fn evidence_kind_to_str(kind: EvidenceKind) -> &'static str {
    use EvidenceKind::*;

    match kind {
        Text => "Text",
        Verse => "Verse",
        Footnote => "Footnote",
        Ocr => "Ocr",
        Image => "Image",
        Generated => "Generated",
    }
}

pub(super) fn str_to_evidence_kind(kind: &str) -> EvidenceKind {
    use EvidenceKind::*;

    match kind {
        "Verse" => Verse,
        "Footnote" => Footnote,
        "Ocr" => Ocr,
        "Image" => Image,
        "Generated" => Generated,
        _ => Text,
    }
}

pub(super) fn image_caption_status_to_str(status: ImageCaptionStatus) -> &'static str {
    match status {
        ImageCaptionStatus::Success => "Success",
        ImageCaptionStatus::Failed => "Failed",
        ImageCaptionStatus::Skipped => "Skipped",
    }
}

pub(super) fn str_to_image_caption_status(status: &str) -> ImageCaptionStatus {
    match status {
        "Success" => ImageCaptionStatus::Success,
        "Skipped" => ImageCaptionStatus::Skipped,
        _ => ImageCaptionStatus::Failed,
    }
}

pub(super) fn chunk_type_to_str(chunk_type: &ChunkType) -> &'static str {
    match chunk_type {
        ChunkType::Child => "Child",
        ChunkType::Parent => "Parent",
    }
}

pub(super) fn str_to_chunk_type(value: &str) -> ChunkType {
    match value {
        "Parent" => ChunkType::Parent,
        _ => ChunkType::Child,
    }
}

pub(super) fn str_to_graph_node_kind(
    value: &str,
    column: usize,
) -> rusqlite::Result<GraphNodeKind> {
    match value {
        "Source" => Ok(GraphNodeKind::Source),
        "Page" => Ok(GraphNodeKind::Page),
        "Section" => Ok(GraphNodeKind::Section),
        "Chunk" => Ok(GraphNodeKind::Chunk),
        "EvidenceUnit" => Ok(GraphNodeKind::EvidenceUnit),
        "ImageArtifact" => Ok(GraphNodeKind::ImageArtifact),
        "GeneratedEntity" => Ok(GraphNodeKind::GeneratedEntity),
        "GeneratedClaim" => Ok(GraphNodeKind::GeneratedClaim),
        _ => Err(invalid_text_value(
            column,
            format!("unknown graph node kind: {value}"),
        )),
    }
}

pub(super) fn str_to_edge_type(value: &str, column: usize) -> rusqlite::Result<EdgeType> {
    match value {
        "contains" | "Contains" => Ok(EdgeType::Contains),
        "derived_from" | "DerivedFrom" => Ok(EdgeType::DerivedFrom),
        "parent" => Ok(EdgeType::Parent),
        "child" => Ok(EdgeType::Child),
        "previous" => Ok(EdgeType::Previous),
        "next" | "Next" => Ok(EdgeType::Next),
        "same_source" => Ok(EdgeType::SameSource),
        "same_page" => Ok(EdgeType::SamePage),
        "section_contains" => Ok(EdgeType::SectionContains),
        "page_contains_image" => Ok(EdgeType::PageContainsImage),
        "image_near_text" => Ok(EdgeType::ImageNearText),
        "markdown_links_to" => Ok(EdgeType::MarkdownLinksTo),
        "footnote_references_verse" => Ok(EdgeType::FootnoteReferencesVerse),
        "generated_depends_on" => Ok(EdgeType::GeneratedDependsOn),
        "generated_implements" => Ok(EdgeType::GeneratedImplements),
        "generated_mentions" => Ok(EdgeType::GeneratedMentions),
        "generated_conflicts_with" => Ok(EdgeType::GeneratedConflictsWith),
        "generated_supports" => Ok(EdgeType::GeneratedSupports),
        "generated_other" => Ok(EdgeType::GeneratedOther),
        _ => Err(invalid_text_value(
            column,
            format!("unknown graph edge type: {value}"),
        )),
    }
}

pub(super) fn invalid_text_value(column: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

pub(super) fn json_from_sql<T>(column: usize, value: &str) -> rusqlite::Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(value).map_err(|error| invalid_text_value(column, error.to_string()))
}
