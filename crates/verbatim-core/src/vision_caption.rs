use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::provider::{ImageDescribeRequest, ImageDescription, ImageInput, VisionModel};
use crate::types::{
    hex_sha256, EvidenceId, EvidenceKind, EvidenceUnit, ImageArtifact, SourceId, SourceLocator,
};

/// Version string included in the deterministic image caption cache key.
pub const VISION_CAPTION_PROMPT_VERSION: &str = "vision-caption-v1";
const VISION_CAPTION_MAX_TOKENS: u32 = 1200;
const VISION_CAPTION_DETAIL: &str = "high";

const VISION_CAPTION_PROMPT: &str = r#"You are generating derived, searchable evidence for one extracted PDF image.
Return only one strict JSON object. Do not wrap it in Markdown.

Required schema:
{
  "type": "diagram|chart|table|screenshot|photo|equation|other",
  "short_caption": "one concise sentence",
  "detailed_description": "specific visual description grounded only in the image",
  "visible_text": ["text visibly present in the image, approximate unless OCR-verified"],
  "key_entities": ["people, labels, organizations, objects, variables, or concepts visible in the image"],
  "relationships": [{"from": "entity", "to": "entity", "label": "visible relationship"}],
  "answerable_questions": ["questions this image could answer"],
  "uncertainties": ["important ambiguity, low-confidence reading, or missing context"]
}

Rules:
- Use empty arrays when a field has no entries.
- Use "other" for unknown image type; never invent a new type string.
- Do not claim visible text is an exact transcription unless the image clearly supports it.
- If text is hard to read, include the uncertainty instead of guessing.
- Do not include fields outside the schema."#;

fn repair_prompt(invalid_json: &str, parse_error: &str) -> String {
    format!(
        r#"The previous response was not valid for the required strict JSON schema.
Return only a corrected JSON object for the same image, with no Markdown and no extra fields.

Parse/validation error:
{parse_error}

Previous response:
{invalid_json}

Required field names are exactly:
type, short_caption, detailed_description, visible_text, key_entities, relationships, answerable_questions, uncertainties."#
    )
}

fn vision_caption_prompt() -> &'static str {
    VISION_CAPTION_PROMPT
}

/// Deterministic hash for the active caption prompt version and body.
pub fn vision_caption_prompt_hash() -> String {
    let prompt_material = format!("{VISION_CAPTION_PROMPT_VERSION}\n{VISION_CAPTION_PROMPT}");
    hex_sha256(prompt_material.as_bytes())
}

/// Stored status for one image caption cache key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageCaptionStatus {
    Success,
    Failed,
    Skipped,
}

/// Closed content type set accepted from the strict caption JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageCaptionContentType {
    Diagram,
    Chart,
    Table,
    Screenshot,
    Photo,
    Equation,
    Other,
}

/// Directed relationship visible in a generated image caption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCaptionRelationship {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// Strict structured caption generated from a PDF image artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCaption {
    #[serde(rename = "type")]
    pub content_type: ImageCaptionContentType,
    pub short_caption: String,
    pub detailed_description: String,
    pub visible_text: Vec<String>,
    pub key_entities: Vec<String>,
    pub relationships: Vec<ImageCaptionRelationship>,
    pub answerable_questions: Vec<String>,
    pub uncertainties: Vec<String>,
}

impl ImageCaption {
    /// Parse one strict JSON caption object and validate semantic fields.
    pub fn parse_strict(raw: &str) -> Result<Self> {
        let caption: Self = serde_json::from_str(raw).with_context(|| "parse caption JSON")?;
        caption.validate()?;
        Ok(caption)
    }

    fn validate(&self) -> Result<()> {
        require_non_blank("short_caption", &self.short_caption)?;
        require_non_blank("detailed_description", &self.detailed_description)?;
        require_string_items("visible_text", &self.visible_text)?;
        require_string_items("key_entities", &self.key_entities)?;
        require_string_items("answerable_questions", &self.answerable_questions)?;
        require_string_items("uncertainties", &self.uncertainties)?;
        for (idx, relationship) in self.relationships.iter().enumerate() {
            require_non_blank(&format!("relationships[{idx}].from"), &relationship.from)?;
            require_non_blank(&format!("relationships[{idx}].to"), &relationship.to)?;
            require_non_blank(&format!("relationships[{idx}].label"), &relationship.label)?;
        }
        Ok(())
    }
}

/// Persisted cache record for one `(image_hash, model, prompt_hash)` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCaptionRecord {
    pub image_hash: String,
    pub model: String,
    pub prompt_version: String,
    pub prompt_hash: String,
    pub status: ImageCaptionStatus,
    pub caption: Option<ImageCaption>,
    pub raw_response: Option<String>,
    pub error_message: Option<String>,
    pub attempt_count: u32,
    pub cache_hits: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptionAttempt {
    pub status: ImageCaptionStatus,
    pub caption: Option<ImageCaption>,
    pub raw_response: Option<String>,
    pub error_message: Option<String>,
    pub attempt_count: u32,
}

impl CaptionAttempt {
    pub(crate) fn success(caption: ImageCaption, raw_response: String, attempt_count: u32) -> Self {
        Self {
            status: ImageCaptionStatus::Success,
            caption: Some(caption),
            raw_response: Some(raw_response),
            error_message: None,
            attempt_count,
        }
    }

    pub(crate) fn failed(
        raw_response: Option<String>,
        error_message: impl Into<String>,
        attempt_count: u32,
    ) -> Self {
        Self {
            status: ImageCaptionStatus::Failed,
            caption: None,
            raw_response,
            error_message: Some(error_message.into()),
            attempt_count,
        }
    }

    pub(crate) fn skipped(error_message: impl Into<String>) -> Self {
        Self {
            status: ImageCaptionStatus::Skipped,
            caption: None,
            raw_response: None,
            error_message: Some(error_message.into()),
            attempt_count: 0,
        }
    }
}

pub(crate) async fn request_image_caption(
    model: &dyn VisionModel,
    image_bytes: &[u8],
    mime_type: &str,
) -> CaptionAttempt {
    let image = ImageInput::data_uri(image_data_uri(image_bytes, mime_type));
    let first = describe(model, image.clone(), vision_caption_prompt().to_string()).await;
    let first_raw = match first {
        Ok(response) => response.text,
        Err(err) => {
            return CaptionAttempt::failed(None, err.to_string(), 1);
        }
    };

    match ImageCaption::parse_strict(&first_raw) {
        Ok(caption) => CaptionAttempt::success(caption, first_raw, 1),
        Err(first_err) => {
            let repair = describe(
                model,
                image,
                repair_prompt(&first_raw, &first_err.to_string()),
            )
            .await;
            let repair_raw = match repair {
                Ok(response) => response.text,
                Err(err) => {
                    return CaptionAttempt::failed(
                        Some(first_raw),
                        format!("repair request failed after malformed JSON: {err}"),
                        2,
                    );
                }
            };
            match ImageCaption::parse_strict(&repair_raw) {
                Ok(caption) => CaptionAttempt::success(caption, repair_raw, 2),
                Err(repair_err) => CaptionAttempt::failed(
                    Some(repair_raw),
                    format!(
                        "caption JSON repair failed after initial parse error ({first_err}): {repair_err}"
                    ),
                    2,
                ),
            }
        }
    }
}

async fn describe(
    model: &dyn VisionModel,
    image: ImageInput,
    prompt: String,
) -> crate::provider::ProviderResult<ImageDescription> {
    model
        .describe_image(
            ImageDescribeRequest::new(image, prompt)
                .with_detail(VISION_CAPTION_DETAIL)
                .with_max_tokens(VISION_CAPTION_MAX_TOKENS),
        )
        .await
}

fn image_data_uri(image_bytes: &[u8], mime_type: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(image_bytes);
    format!("data:{mime_type};base64,{encoded}")
}

fn caption_evidence_id(
    image_id: &crate::types::ImageId,
    model: &str,
    prompt_hash: &str,
) -> EvidenceId {
    let key = format!("{}\n{model}\n{prompt_hash}", image_id.0);
    let digest = hex_sha256(key.as_bytes());
    EvidenceId(format!("{}:caption:{}", image_id.0, &digest[..16]))
}

pub(crate) fn caption_derived_evidence(
    source_id: &SourceId,
    artifact: &ImageArtifact,
    caption: &ImageCaption,
    model: &str,
    prompt_hash: &str,
    position: u32,
) -> EvidenceUnit {
    let locator = SourceLocator::PdfImage {
        page: artifact.page,
        image_index: artifact.image_index,
        bbox: artifact.bbox.clone(),
    };
    let text = caption_evidence_text(artifact, &locator, caption, model, prompt_hash);
    EvidenceUnit {
        id: caption_evidence_id(&artifact.image_id, model, prompt_hash),
        source_id: source_id.clone(),
        kind: EvidenceKind::Generated,
        locator,
        text_hash: hex_sha256(text.as_bytes()),
        text,
        heading_path: vec!["Generated image captions".to_string()],
        position,
    }
}

fn caption_evidence_text(
    artifact: &ImageArtifact,
    locator: &SourceLocator,
    caption: &ImageCaption,
    model: &str,
    prompt_hash: &str,
) -> String {
    let mut text = format!(
        "Generated image caption (derived evidence; not original source text and not exact OCR). Original image evidence: {}. Original locator: {locator}. Model: {model}. Prompt hash: {prompt_hash}. Content type: {:?}. Short caption: {}. Detailed description: {}.",
        artifact.evidence_id.0,
        caption.content_type,
        caption.short_caption,
        caption.detailed_description
    );
    append_string_list(
        &mut text,
        "Visible text noted by the vision model (not OCR-verified)",
        &caption.visible_text,
    );
    append_string_list(&mut text, "Key entities", &caption.key_entities);
    if !caption.relationships.is_empty() {
        text.push_str(" Relationships: ");
        text.push_str(
            &caption
                .relationships
                .iter()
                .map(|relationship| {
                    format!(
                        "{} -> {} ({})",
                        relationship.from, relationship.to, relationship.label
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        );
        text.push('.');
    }
    append_string_list(
        &mut text,
        "Answerable questions",
        &caption.answerable_questions,
    );
    append_string_list(&mut text, "Uncertainties", &caption.uncertainties);
    text
}

fn append_string_list(text: &mut String, label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    text.push(' ');
    text.push_str(label);
    text.push_str(": ");
    text.push_str(&items.join("; "));
    text.push('.');
}

fn require_string_items(field: &str, items: &[String]) -> Result<()> {
    for (idx, item) in items.iter().enumerate() {
        require_non_blank(&format!("{field}[{idx}]"), item)?;
    }
    Ok(())
}

fn require_non_blank(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be blank");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> &'static str {
        r#"{
  "type": "diagram",
  "short_caption": "A flow diagram.",
  "detailed_description": "A source connects to an index.",
  "visible_text": ["Source", "Index"],
  "key_entities": ["Source", "Index"],
  "relationships": [{"from": "Source", "to": "Index", "label": "feeds"}],
  "answerable_questions": ["What feeds the index?"],
  "uncertainties": []
}"#
    }

    #[test]
    fn strict_caption_json_accepts_expected_schema() {
        let caption = ImageCaption::parse_strict(valid_json()).unwrap();

        assert_eq!(caption.content_type, ImageCaptionContentType::Diagram);
        assert_eq!(caption.visible_text, vec!["Source", "Index"]);
    }

    #[test]
    fn strict_caption_json_rejects_unknown_fields() {
        let json = valid_json().replace("\n}", ",\n  \"extra\": true\n}");

        let err = ImageCaption::parse_strict(&json).unwrap_err();

        assert!(err
            .chain()
            .any(|cause| cause.to_string().contains("unknown field")));
    }

    #[test]
    fn strict_caption_json_rejects_unknown_content_type() {
        let json = valid_json().replace("\"diagram\"", "\"infographic\"");

        let err = ImageCaption::parse_strict(&json).unwrap_err();

        assert!(err
            .chain()
            .any(|cause| cause.to_string().contains("unknown variant")));
    }

    #[test]
    fn data_uri_uses_base64_payload() {
        assert_eq!(
            image_data_uri(b"abc", "image/png"),
            "data:image/png;base64,YWJj"
        );
    }
}
