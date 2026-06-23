use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::config::{ChatConfig, ChatVisionAttachmentConfig, VerifierConfig};
use crate::provider::openai_compatible::OpenAiCompatibleChatModel;
use crate::provider::{ChatContentPart, ChatMessage, ChatModel, ChatRequest, ImageUrl};
use crate::types::{
    CitationRef, EvidenceId, EvidenceKind, EvidenceUnit, ImageArtifact, RetrievalResult,
    SourceLocator,
};

pub struct Generator {
    chat_model: Arc<dyn ChatModel>,
    verifier_enabled: bool,
    vision_attachments: ChatVisionAttachmentConfig,
}

pub struct GenerationResult {
    pub answer: String,
    pub citations: Vec<CitationRef>,
    pub verified: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GenerationContext {
    pub image_artifacts: Vec<ImageArtifact>,
    pub image_attachments: Vec<ImageAttachment>,
}

impl GenerationContext {
    pub fn new(
        image_artifacts: Vec<ImageArtifact>,
        image_attachments: Vec<ImageAttachment>,
    ) -> Self {
        Self {
            image_artifacts,
            image_attachments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    pub evidence_id: EvidenceId,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

impl Generator {
    pub fn new(chat: &ChatConfig, verifier: &VerifierConfig) -> Self {
        Self {
            chat_model: Arc::new(OpenAiCompatibleChatModel::from_config(chat)),
            verifier_enabled: verifier.enabled,
            vision_attachments: chat.vision_attachments.clone(),
        }
    }

    pub fn with_chat_model(
        chat_model: Arc<dyn ChatModel>,
        verifier_enabled: bool,
        vision_attachments: ChatVisionAttachmentConfig,
    ) -> Self {
        Self {
            chat_model,
            verifier_enabled,
            vision_attachments,
        }
    }

    pub async fn generate(
        &self,
        question: &str,
        results: &[RetrievalResult],
    ) -> Result<GenerationResult> {
        self.generate_with_context(question, results, &GenerationContext::default())
            .await
    }

    pub async fn generate_with_context(
        &self,
        question: &str,
        results: &[RetrievalResult],
        context: &GenerationContext,
    ) -> Result<GenerationResult> {
        let source_pack = build_source_pack(
            results,
            context,
            self.vision_attachments.can_attach_images(),
        );

        let system_prompt = SYSTEM_PROMPT;
        let user_prompt = format!(
            "SOURCE PACK:\n{}\n\nUSER QUESTION:\n{question}",
            source_pack.text
        );

        let raw_answer = self
            .chat(system_prompt, &user_prompt, &source_pack.attachments)
            .await?;

        let citations = extract_citations(&raw_answer, &source_pack.evidence_refs);
        let citation_attachments =
            relevant_attachments_for_citations(&citations, &source_pack.attachments);
        if self.verifier_enabled {
            let verification = self
                .verify_with_attachments(question, &raw_answer, &citations, &citation_attachments)
                .await?;
            return self
                .apply_verification(
                    question,
                    &raw_answer,
                    citations,
                    &citation_attachments,
                    verification,
                )
                .await;
        }

        let answer = render_answer(&raw_answer, &citations);

        Ok(GenerationResult {
            answer,
            citations,
            verified: false,
        })
    }

    /// Generate an answer while forwarding provider deltas to `on_delta`.
    ///
    /// Streaming emits raw model text before post-generation verification can
    /// revise it, so this path intentionally returns `verified = false`.
    /// Call `generate_with_context` when a verified final answer is required.
    pub async fn generate_streaming_with_context<F>(
        &self,
        question: &str,
        results: &[RetrievalResult],
        context: &GenerationContext,
        mut on_delta: F,
    ) -> Result<GenerationResult>
    where
        F: FnMut(&str) -> Result<()> + Send,
    {
        let source_pack = build_source_pack(
            results,
            context,
            self.vision_attachments.can_attach_images(),
        );

        let user_prompt = format!(
            "SOURCE PACK:\n{}\n\nUSER QUESTION:\n{question}",
            source_pack.text
        );
        let raw_answer = self
            .stream_chat(
                SYSTEM_PROMPT,
                &user_prompt,
                &source_pack.attachments,
                |delta| on_delta(delta),
            )
            .await?;

        let citations = extract_citations(&raw_answer, &source_pack.evidence_refs);
        let answer = render_answer(&raw_answer, &citations);

        Ok(GenerationResult {
            answer,
            citations,
            verified: false,
        })
    }

    pub async fn verify(
        &self,
        question: &str,
        answer: &str,
        citations: &[CitationRef],
    ) -> Result<VerificationResult> {
        self.verify_with_attachments(question, answer, citations, &[])
            .await
    }

    async fn verify_with_attachments(
        &self,
        question: &str,
        answer: &str,
        citations: &[CitationRef],
        attachments: &[SourcePackAttachment],
    ) -> Result<VerificationResult> {
        let sources_json = verifier_source_inputs(citations, attachments);

        let prompt = format!(
            "Verify this answer against the cited sources.\n\n\
             Question: {question}\n\n\
             Answer: {answer}\n\n\
             Sources: {sources}\n\n\
             Verification rules:\n\
             - original_text supports claims grounded in document text.\n\
             - ocr_text supports claims grounded in OCR-derived document text; consider OCR confidence and do not treat it as generated prose.\n\
             - image_caption_generated is generated derived evidence, not original PDF text; use it conservatively and revise over-strong wording when needed.\n\
             - image_artifact metadata supports image location/artifact facts; visual content claims need either caption support or an included vision_attachment.\n\
             - Prefer image_artifact with an included vision_attachment as the strongest visual citation when available.\n\
             - Unsupported visual claims with no image/caption support must be revise or fail, never pass.\n\n\
             Output JSON with this schema:\n\
             {{\"verdict\": \"pass|revise|fail\", \
             \"unsupported_claims\": [\"claim text\"]}}",
            sources = serde_json::to_string_pretty(&sources_json)?
        );

        let response = self
            .chat(
                "You are a citation verification system. Output only valid JSON.",
                &prompt,
                attachments,
            )
            .await?;

        let cleaned = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        serde_json::from_str::<VerificationResult>(cleaned)
            .context("verifier returned invalid JSON")
    }

    async fn apply_verification(
        &self,
        question: &str,
        raw_answer: &str,
        citations: Vec<CitationRef>,
        attachments: &[SourcePackAttachment],
        verification: VerificationResult,
    ) -> Result<GenerationResult> {
        match verification.verdict {
            VerificationVerdict::Pass => Ok(GenerationResult {
                answer: render_answer(raw_answer, &citations),
                citations,
                verified: true,
            }),
            VerificationVerdict::Revise => {
                let revised = self
                    .revise_answer(
                        question,
                        raw_answer,
                        &citations,
                        attachments,
                        &verification.unsupported_claims,
                    )
                    .await?;
                if is_insufficient_answer(&revised) {
                    return Ok(insufficient_generation());
                }
                let revised_citations = filter_citations_for_answer(&revised, &citations);
                let revised_attachments =
                    relevant_attachments_for_citations(&revised_citations, attachments);
                let second_pass = self
                    .verify_with_attachments(
                        question,
                        &revised,
                        &revised_citations,
                        &revised_attachments,
                    )
                    .await?;
                if second_pass.verdict == VerificationVerdict::Pass {
                    Ok(GenerationResult {
                        answer: render_answer(&revised, &revised_citations),
                        citations: revised_citations,
                        verified: true,
                    })
                } else {
                    Ok(insufficient_generation())
                }
            }
            _ => Ok(insufficient_generation()),
        }
    }

    async fn revise_answer(
        &self,
        question: &str,
        answer: &str,
        citations: &[CitationRef],
        attachments: &[SourcePackAttachment],
        unsupported_claims: &[String],
    ) -> Result<String> {
        let sources_json = verifier_source_inputs(citations, attachments);
        let prompt = format!(
            "Revise the answer so every remaining factual claim is directly supported by the cited sources.\n\n\
             Question: {question}\n\n\
             Original answer: {answer}\n\n\
             Unsupported claims to remove: {unsupported}\n\n\
             Sources: {sources}\n\n\
             Preserve citation labels only for sources still used in the revised answer. \
             Treat ocr_text as OCR-derived source text, not generated prose. \
             Treat image_caption_generated as caption-only derived evidence unless a vision_attachment is included. \
             Remove visual claims that lack caption support or included image evidence.\n\n\
             If the sources are insufficient, output exactly: Evidence insufficient to answer this question.",
            unsupported = serde_json::to_string_pretty(unsupported_claims)?,
            sources = serde_json::to_string_pretty(&sources_json)?
        );
        self.chat(
            "You revise answers to remove unsupported claims. Output only the revised answer.",
            &prompt,
            attachments,
        )
        .await
    }

    async fn chat(
        &self,
        system: &str,
        user: &str,
        attachments: &[SourcePackAttachment],
    ) -> Result<String> {
        let user_message = if attachments.is_empty() {
            ChatMessage::user(user)
        } else {
            ChatMessage::user_parts(chat_parts_with_images(
                user,
                attachments,
                &self.vision_attachments,
            ))
        };

        let response = self
            .chat_model
            .chat(ChatRequest::new(vec![
                ChatMessage::system(system),
                user_message,
            ]))
            .await
            .context("chat completion failed")?;
        Ok(response.content)
    }

    async fn stream_chat<F>(
        &self,
        system: &str,
        user: &str,
        attachments: &[SourcePackAttachment],
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()> + Send,
    {
        let user_message = if attachments.is_empty() {
            ChatMessage::user(user)
        } else {
            ChatMessage::user_parts(chat_parts_with_images(
                user,
                attachments,
                &self.vision_attachments,
            ))
        };

        let mut stream = self
            .chat_model
            .stream_chat(ChatRequest::new(vec![
                ChatMessage::system(system),
                user_message,
            ]))
            .await
            .context("streaming chat completion failed")?;

        let mut answer = String::new();
        while let Some(event) = stream.next().await {
            let event = event.context("streaming chat completion failed")?;
            if event.delta.is_empty() {
                continue;
            }
            on_delta(&event.delta)?;
            answer.push_str(&event.delta);
        }

        Ok(answer)
    }
}

fn insufficient_generation() -> GenerationResult {
    GenerationResult {
        answer: "Evidence insufficient to answer this question.".into(),
        citations: Vec::new(),
        verified: false,
    }
}

fn is_insufficient_answer(answer: &str) -> bool {
    answer.trim() == "Evidence insufficient to answer this question."
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationVerdict {
    Pass,
    Revise,
    Fail,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationResult {
    pub verdict: VerificationVerdict,
    #[serde(default)]
    pub unsupported_claims: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct VerifierEvidenceInput {
    id: String,
    evidence_id: String,
    source_id: String,
    kind: &'static str,
    locator: VerifierLocatorInput,
    text: String,
    provenance: VerifierEvidenceProvenance,
    visual_support: VerifierVisualSupport,
}

#[derive(Debug, Clone, Serialize)]
struct VerifierLocatorInput {
    display: String,
    structured: SourceLocator,
}

#[derive(Debug, Clone, Serialize)]
struct VerifierEvidenceProvenance {
    summary: &'static str,
    derived_from: Option<String>,
    derivation_chain: Vec<VerifierDerivationStep>,
}

#[derive(Debug, Clone, Serialize)]
struct VerifierDerivationStep {
    evidence_id: String,
    source_id: String,
    kind: &'static str,
    locator: VerifierLocatorInput,
    relation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct VerifierVisualSupport {
    support_level: &'static str,
    vision_attachment: &'static str,
    image_evidence_id: Option<String>,
    caution: &'static str,
}

struct SourcePack {
    text: String,
    evidence_refs: Vec<EvidenceRef>,
    attachments: Vec<SourcePackAttachment>,
}

fn build_source_pack(
    results: &[RetrievalResult],
    context: &GenerationContext,
    include_attachments: bool,
) -> SourcePack {
    let mut pack = String::new();
    let mut evidence_refs = Vec::new();
    let mut counter = 1;

    let mut seen_evidence: HashMap<String, usize> = HashMap::new();

    for result in results {
        for eu in &result.evidence_units {
            if seen_evidence.contains_key(&eu.id.0) {
                continue;
            }

            let eid_label = format!("E{counter}");
            seen_evidence.insert(eu.id.0.clone(), counter);
            let artifact = context.image_artifact_for(eu).cloned();
            let attachment = include_attachments
                .then(|| context.image_attachment_for(eu).cloned())
                .flatten();

            push_source_pack_entry(
                &mut pack,
                &eid_label,
                eu,
                artifact.as_ref(),
                attachment.as_ref(),
            );

            evidence_refs.push(EvidenceRef {
                label: eid_label,
                evidence: eu.clone(),
            });

            counter += 1;
        }
    }

    let attachments = if include_attachments {
        source_pack_attachments(&evidence_refs, context)
    } else {
        Vec::new()
    };

    SourcePack {
        text: pack,
        evidence_refs,
        attachments,
    }
}

struct EvidenceRef {
    label: String,
    evidence: EvidenceUnit,
}

#[derive(Clone)]
struct SourcePackAttachment {
    labels: Vec<String>,
    evidence_id: EvidenceId,
    locator: SourceLocator,
    payload: ImageAttachment,
}

fn source_pack_attachments(
    evidence_refs: &[EvidenceRef],
    context: &GenerationContext,
) -> Vec<SourcePackAttachment> {
    let mut attachments = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for payload in &context.image_attachments {
        if !seen.insert(payload.evidence_id.0.clone()) {
            continue;
        }
        let linked_refs: Vec<&EvidenceRef> = evidence_refs
            .iter()
            .filter(|eref| {
                image_artifact_evidence_id(&eref.evidence)
                    .is_some_and(|evidence_id| evidence_id == &payload.evidence_id)
            })
            .collect();
        let Some(first_ref) = linked_refs.first() else {
            continue;
        };

        attachments.push(SourcePackAttachment {
            labels: linked_refs.iter().map(|eref| eref.label.clone()).collect(),
            evidence_id: payload.evidence_id.clone(),
            locator: first_ref.evidence.locator.clone(),
            payload: payload.clone(),
        });
    }

    attachments
}

fn relevant_attachments_for_citations(
    citations: &[CitationRef],
    attachments: &[SourcePackAttachment],
) -> Vec<SourcePackAttachment> {
    let cited_labels: HashSet<&str> = citations
        .iter()
        .map(|citation| citation.label.as_str())
        .collect();

    attachments
        .iter()
        .filter_map(|attachment| {
            let labels: Vec<String> = attachment
                .labels
                .iter()
                .filter(|label| cited_labels.contains(label.as_str()))
                .cloned()
                .collect();

            if labels.is_empty() {
                None
            } else {
                Some(SourcePackAttachment {
                    labels,
                    evidence_id: attachment.evidence_id.clone(),
                    locator: attachment.locator.clone(),
                    payload: attachment.payload.clone(),
                })
            }
        })
        .collect()
}

fn filter_citations_for_answer(answer: &str, citations: &[CitationRef]) -> Vec<CitationRef> {
    let cited_labels = cited_labels(answer);
    citations
        .iter()
        .filter(|citation| cited_labels.contains(&citation.label))
        .cloned()
        .collect()
}

fn verifier_source_inputs(
    citations: &[CitationRef],
    attachments: &[SourcePackAttachment],
) -> Vec<VerifierEvidenceInput> {
    citations
        .iter()
        .map(|citation| VerifierEvidenceInput {
            id: citation.label.clone(),
            evidence_id: citation.evidence_id.0.clone(),
            source_id: citation.source_id.0.clone(),
            kind: citation_kind_label(citation.kind, citation.derived_from.as_ref()),
            locator: verifier_locator(&citation.locator),
            text: citation.text_preview.clone(),
            provenance: verifier_provenance(citation),
            visual_support: verifier_visual_support(citation, attachments),
        })
        .collect()
}

fn verifier_provenance(citation: &CitationRef) -> VerifierEvidenceProvenance {
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
        derivation_chain: verifier_derivation_chain(citation),
    }
}

fn verifier_derivation_chain(citation: &CitationRef) -> Vec<VerifierDerivationStep> {
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
                evidence_id: citation.evidence_id.0.clone(),
                source_id: citation.source_id.0.clone(),
                kind: "image_caption_generated",
                locator: verifier_locator(&citation.locator),
                relation: "generated_caption_from_image",
            },
        ],
        _ => vec![VerifierDerivationStep {
            evidence_id: citation.evidence_id.0.clone(),
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

impl GenerationContext {
    fn image_artifact_for(&self, evidence: &EvidenceUnit) -> Option<&ImageArtifact> {
        let evidence_id = image_artifact_evidence_id(evidence)?;
        self.image_artifacts
            .iter()
            .find(|artifact| artifact.evidence_id == *evidence_id)
    }

    fn image_attachment_for(&self, evidence: &EvidenceUnit) -> Option<&ImageAttachment> {
        let evidence_id = image_artifact_evidence_id(evidence)?;
        self.image_attachments
            .iter()
            .find(|attachment| attachment.evidence_id == *evidence_id)
    }
}

pub fn image_artifact_evidence_id(evidence: &EvidenceUnit) -> Option<&EvidenceId> {
    match evidence.kind {
        EvidenceKind::Image => Some(&evidence.id),
        EvidenceKind::Generated => evidence.derived_from.as_ref(),
        EvidenceKind::Text | EvidenceKind::Ocr => None,
    }
}

pub fn select_image_attachments<F>(
    results: &[RetrievalResult],
    image_artifacts: &[ImageArtifact],
    config: &ChatVisionAttachmentConfig,
    mut load_image_bytes: F,
) -> Result<Vec<ImageAttachment>>
where
    F: FnMut(&ImageArtifact) -> Result<Vec<u8>>,
{
    if !config.can_attach_images() {
        return Ok(Vec::new());
    }

    let mut attachments = Vec::new();
    let mut total_bytes = 0usize;

    for evidence_id in selected_image_evidence_ids(results) {
        if attachments.len() >= config.max_images {
            break;
        }
        let Some(artifact) = image_artifacts
            .iter()
            .find(|artifact| artifact.evidence_id == evidence_id)
        else {
            continue;
        };

        let bytes = load_image_bytes(artifact)?;
        if total_bytes.saturating_add(bytes.len()) > config.max_total_bytes {
            continue;
        }

        total_bytes += bytes.len();
        attachments.push(ImageAttachment {
            evidence_id,
            mime_type: artifact.mime_type.clone(),
            bytes,
        });
    }

    Ok(attachments)
}

pub fn selected_image_evidence_ids(results: &[RetrievalResult]) -> Vec<EvidenceId> {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();

    for result in results {
        for evidence in &result.evidence_units {
            let Some(evidence_id) = image_artifact_evidence_id(evidence) else {
                continue;
            };
            if seen.insert(evidence_id.0.clone()) {
                selected.push(evidence_id.clone());
            }
        }
    }

    selected
}

fn push_source_pack_entry(
    pack: &mut String,
    label: &str,
    evidence: &EvidenceUnit,
    artifact: Option<&ImageArtifact>,
    attachment: Option<&ImageAttachment>,
) {
    let kind = source_pack_kind_label(evidence);
    let derived = evidence
        .derived_from
        .as_ref()
        .map(|id| format!(" | derived_from={}", id.0))
        .unwrap_or_default();

    pack.push_str(&format!(
        "[{label} | {kind} | {locator}{derived}]\n",
        locator = evidence.locator
    ));

    match kind {
        "original_text" => {
            pack.push_str("original_text:\n");
            pack.push_str(&evidence.text);
            pack.push('\n');
        }
        "ocr_text" => {
            pack.push_str("ocr_text:\n");
            pack.push_str(&evidence.text);
            pack.push('\n');
            pack.push_str(
                "provenance: OCR-derived source text with structured OCR locator and confidence metadata; not generated text.\n",
            );
        }
        "image_caption_generated" => {
            pack.push_str("image_caption_generated:\n");
            pack.push_str(&evidence.text);
            pack.push('\n');
            pack.push_str(
                "provenance: generated image caption; derived evidence, not original PDF text.\n",
            );
        }
        "image_artifact" => {
            pack.push_str("image_artifact_text:\n");
            pack.push_str(&evidence.text);
            pack.push('\n');
            pack.push_str(
                "provenance: original extracted image artifact metadata, not PDF body text.\n",
            );
        }
        _ => {
            pack.push_str("generated_text:\n");
            pack.push_str(&evidence.text);
            pack.push('\n');
            pack.push_str("provenance: generated derived evidence, not original source text.\n");
        }
    }

    if let Some(artifact) = artifact {
        pack.push_str("image_artifact_metadata: ");
        pack.push_str(&format!(
            "image_id={}; evidence_id={}; path={}; mime_type={}; dimensions={}x{}; page={}; image_index={}",
            artifact.image_id.0,
            artifact.evidence_id.0,
            artifact.relative_path.display(),
            artifact.mime_type,
            artifact.width,
            artifact.height,
            artifact.page,
            artifact.image_index
        ));
        if let Some(bbox) = &artifact.bbox {
            pack.push_str(&format!(
                "; bbox=[{:.2},{:.2},{:.2},{:.2}]",
                bbox.x0, bbox.y0, bbox.x1, bbox.y1
            ));
        }
        pack.push('\n');
    }

    if image_artifact_evidence_id(evidence).is_some() {
        let status = if attachment.is_some() {
            "included"
        } else {
            "not_included"
        };
        pack.push_str(&format!("vision_attachment: {status}\n"));
    }

    pack.push('\n');
}

fn source_pack_kind_label(evidence: &EvidenceUnit) -> &'static str {
    match evidence.kind {
        EvidenceKind::Text => "original_text",
        EvidenceKind::Ocr => "ocr_text",
        EvidenceKind::Image => "image_artifact",
        EvidenceKind::Generated if evidence.derived_from.is_some() => "image_caption_generated",
        EvidenceKind::Generated => "generated",
    }
}

fn extract_citations(answer: &str, evidence_refs: &[EvidenceRef]) -> Vec<CitationRef> {
    let mut citations = Vec::new();
    let mut seen = HashSet::new();
    let cited_labels = cited_labels(answer);

    for eref in evidence_refs {
        if cited_labels.contains(&eref.label) && seen.insert(eref.label.clone()) {
            citations.push(CitationRef {
                label: eref.label.clone(),
                evidence_id: eref.evidence.id.clone(),
                source_id: eref.evidence.source_id.clone(),
                kind: eref.evidence.kind,
                derived_from: eref.evidence.derived_from.clone(),
                locator: eref.evidence.locator.clone(),
                text_preview: eref.evidence.text.chars().take(200).collect(),
            });
        }
    }

    citations
}

fn render_answer(raw: &str, citations: &[CitationRef]) -> String {
    let mut output = raw.to_string();

    output.push_str("\n\nReferences:\n");
    for cite in citations {
        let kind = citation_kind_label(cite.kind, cite.derived_from.as_ref());
        let derived = cite
            .derived_from
            .as_ref()
            .map(|id| format!("; derived from {}", id.0))
            .unwrap_or_default();
        output.push_str(&format!(
            "[{}] {}: {}{}\n",
            cite.label, kind, cite.locator, derived
        ));
    }

    output
}

fn cited_labels(answer: &str) -> HashSet<String> {
    let mut labels = HashSet::new();
    let mut rest = answer;

    while let Some(start) = rest.find('[') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find(']') else {
            break;
        };
        let inside = &after_start[..end];
        for token in inside.split(|c: char| c == ',' || c.is_whitespace()) {
            let token = token.trim();
            if is_evidence_label(token) {
                labels.insert(token.to_string());
            }
        }
        rest = &after_start[end + 1..];
    }

    labels
}

fn is_evidence_label(token: &str) -> bool {
    token
        .strip_prefix('E')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn citation_kind_label(kind: EvidenceKind, derived_from: Option<&EvidenceId>) -> &'static str {
    match kind {
        EvidenceKind::Text => "original_text",
        EvidenceKind::Ocr => "ocr_text",
        EvidenceKind::Image => "image_artifact",
        EvidenceKind::Generated if derived_from.is_some() => "image_caption_generated",
        EvidenceKind::Generated => "generated",
    }
}

fn chat_parts_with_images(
    user_prompt: &str,
    attachments: &[SourcePackAttachment],
    config: &ChatVisionAttachmentConfig,
) -> Vec<ChatContentPart> {
    let mut parts = vec![ChatContentPart::Text {
        text: user_prompt.to_string(),
    }];
    let detail = config.detail_value();

    for attachment in attachments {
        let labels = attachment
            .labels
            .iter()
            .map(|label| format!("[{label}]"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(ChatContentPart::Text {
            text: format!(
                "Attached image evidence {labels}: {} (original image evidence id: {}).",
                attachment.locator, attachment.evidence_id.0
            ),
        });
        parts.push(ChatContentPart::ImageUrl {
            image_url: ImageUrl {
                url: image_data_uri(&attachment.payload.bytes, &attachment.payload.mime_type),
                detail: detail.clone(),
            },
        });
    }

    parts
}

fn image_data_uri(image_bytes: &[u8], mime_type: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(image_bytes);
    format!("data:{mime_type};base64,{encoded}")
}

const SYSTEM_PROMPT: &str = "\
You are answering questions about documents.

Rules:
1. Use ONLY the provided SOURCE PACK.
2. Every factual claim must cite one or more source ids like [E1].
3. Do not cite sources that do not directly support the sentence.
4. If the SOURCE PACK does not contain enough evidence, say so.
5. Do not use outside knowledge.
6. Do not invent page numbers, paragraph numbers, quotations, or citations.
7. Treat ocr_text entries as OCR-derived source text with confidence caveats, not generated prose.
8. Treat image_caption_generated entries as derived evidence, not original PDF text.
9. Prefer original image_artifact locators for visual claims when an image artifact is available.
10. Cite image_artifact entries only for image artifact facts or visual content actually attached in this request.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::StreamExt;

    use crate::provider::{
        ChatContentPart, ChatMessageContent, ChatModel, ChatRequest, ChatResponse, ChatStream,
        ChatStreamEvent, ProviderResult,
    };
    use crate::types::{
        BBox, Chunk, ChunkId, ChunkType, EvidenceId, EvidenceKind, ImageId, OcrLocatorMetadata,
        OcrProfile, RetrievalProvenance, SourceId, SourceLocator,
    };

    struct RecordingChatModel {
        responses: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl RecordingChatModel {
        fn new(response: impl Into<String>) -> Self {
            Self::with_responses([response])
        }

        fn with_responses<I, S>(responses: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            Self {
                responses: Mutex::new(responses.into_iter().map(Into::into).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatModel for RecordingChatModel {
        async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
            self.requests.lock().unwrap().push(req);
            let content = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("recording chat model response");
            Ok(ChatResponse {
                content,
                finish_reason: None,
                usage: None,
            })
        }

        async fn stream_chat(&self, _req: ChatRequest) -> ProviderResult<ChatStream> {
            Ok(futures::stream::empty().boxed())
        }
    }

    struct StreamingChatModel {
        chunks: Mutex<VecDeque<Vec<String>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl StreamingChatModel {
        fn new<I, S>(chunks: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            Self {
                chunks: Mutex::new(vec![chunks.into_iter().map(Into::into).collect()].into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatModel for StreamingChatModel {
        async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
            self.requests.lock().unwrap().push(req);
            Ok(ChatResponse {
                content: String::new(),
                finish_reason: None,
                usage: None,
            })
        }

        async fn stream_chat(&self, req: ChatRequest) -> ProviderResult<ChatStream> {
            self.requests.lock().unwrap().push(req);
            let chunks = self
                .chunks
                .lock()
                .unwrap()
                .pop_front()
                .expect("streaming chat model chunks");
            Ok(futures::stream::iter(chunks.into_iter().map(|delta| {
                Ok(ChatStreamEvent {
                    delta,
                    finish_reason: None,
                })
            }))
            .boxed())
        }
    }

    fn assert_request_has_image_part(request: &ChatRequest, expected_label_text: &str) {
        match &request.messages[1].content {
            ChatMessageContent::Parts(parts) => {
                assert!(parts.iter().any(|part| {
                    matches!(
                        part,
                        ChatContentPart::Text { text } if text.contains(expected_label_text)
                    )
                }));
                assert!(parts
                    .iter()
                    .any(|part| matches!(part, ChatContentPart::ImageUrl { .. })));
            }
            ChatMessageContent::Text(_) => panic!("request should include image parts"),
        }
    }

    fn assert_request_is_text_only(request: &ChatRequest) {
        match &request.messages[1].content {
            ChatMessageContent::Text(_) => {}
            ChatMessageContent::Parts(_) => panic!("request should be text-only"),
        }
    }

    fn sample_results() -> Vec<RetrievalResult> {
        vec![RetrievalResult {
            chunk_id: ChunkId("c1".into()),
            score: 0.9,
            chunk: Chunk {
                id: ChunkId("c1".into()),
                source_id: SourceId("src".into()),
                text: "sample".into(),
                context_text: None,
                token_count: 10,
                chunk_type: ChunkType::Parent,
                parent_chunk_id: None,
                heading_path: vec![],
                evidence_unit_ids: vec![],
            },
            evidence_units: vec![
                EvidenceUnit {
                    id: EvidenceId("ev-1".into()),
                    source_id: SourceId("src".into()),
                    kind: EvidenceKind::Text,
                    derived_from: None,
                    locator: SourceLocator::Pdf {
                        page: 42,
                        paragraph: 3,
                        bbox: None,
                    },
                    text: "Freedom is defined as...".into(),
                    text_hash: "h1".into(),
                    heading_path: vec!["Chapter 2".into()],
                    position: 0,
                },
                EvidenceUnit {
                    id: EvidenceId("ev-2".into()),
                    source_id: SourceId("src".into()),
                    kind: EvidenceKind::Text,
                    derived_from: None,
                    locator: SourceLocator::Document {
                        path_or_url: "doc.md".into(),
                        line_start: 10,
                        line_end: Some(15),
                    },
                    text: "The author argues that...".into(),
                    text_hash: "h2".into(),
                    heading_path: vec![],
                    position: 1,
                },
            ],
            provenance: RetrievalProvenance::seed(1, ChunkId("c1".into()), SourceId("src".into())),
        }]
    }

    fn sample_bbox() -> BBox {
        BBox {
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        }
    }

    fn sample_image_artifact() -> ImageArtifact {
        ImageArtifact {
            image_id: ImageId("img-1".into()),
            source_id: SourceId("src".into()),
            evidence_id: EvidenceId("img-1".into()),
            relative_path: PathBuf::from("image-artifacts/src/img-1.png"),
            content_hash: "hash-1".into(),
            mime_type: "image/png".into(),
            width: 640,
            height: 480,
            page: 7,
            image_index: 2,
            bbox: Some(sample_bbox()),
        }
    }

    fn sample_image_caption_results() -> Vec<RetrievalResult> {
        let locator = SourceLocator::PdfImage {
            page: 7,
            image_index: 2,
            bbox: Some(sample_bbox()),
        };
        vec![RetrievalResult {
            chunk_id: ChunkId("caption-child".into()),
            score: 0.95,
            chunk: Chunk {
                id: ChunkId("caption-child".into()),
                source_id: SourceId("src".into()),
                text: "captionneedle".into(),
                context_text: None,
                token_count: 12,
                chunk_type: ChunkType::Child,
                parent_chunk_id: None,
                heading_path: vec![],
                evidence_unit_ids: vec![EvidenceId("cap-1".into())],
            },
            evidence_units: vec![
                EvidenceUnit {
                    id: EvidenceId("cap-1".into()),
                    source_id: SourceId("src".into()),
                    kind: EvidenceKind::Generated,
                    derived_from: Some(EvidenceId("img-1".into())),
                    locator: locator.clone(),
                    text: "Generated image caption: chartneedle appears in the diagram.".into(),
                    text_hash: "caption-hash".into(),
                    heading_path: vec!["Generated image captions".into()],
                    position: 10,
                },
                EvidenceUnit {
                    id: EvidenceId("img-1".into()),
                    source_id: SourceId("src".into()),
                    kind: EvidenceKind::Image,
                    derived_from: None,
                    locator,
                    text: "Image evidence at PDF p.7, image 2. Artifact path: image-artifacts/src/img-1.png.".into(),
                    text_hash: "image-hash".into(),
                    heading_path: vec![],
                    position: 9,
                },
            ],
            provenance: RetrievalProvenance::seed(
                1,
                ChunkId("caption-child".into()),
                SourceId("src".into()),
            ),
        }]
    }

    fn sample_ocr_results() -> Vec<RetrievalResult> {
        let source_id = SourceId("src".into());
        let evidence_id = EvidenceId("ocr-1".into());
        let chunk_id = ChunkId("ocr-child".into());
        vec![RetrievalResult {
            chunk_id: chunk_id.clone(),
            score: 0.99,
            chunk: Chunk {
                id: chunk_id.clone(),
                source_id: source_id.clone(),
                text: "ocrneedle scanned invoice total".into(),
                context_text: None,
                token_count: 5,
                chunk_type: ChunkType::Child,
                parent_chunk_id: None,
                heading_path: vec![],
                evidence_unit_ids: vec![evidence_id.clone()],
            },
            evidence_units: vec![EvidenceUnit {
                id: evidence_id,
                source_id: source_id.clone(),
                kind: EvidenceKind::Ocr,
                derived_from: None,
                locator: SourceLocator::PdfOcr {
                    page: 3,
                    page_label: Some("iii".into()),
                    line_index: 7,
                    word_index: None,
                    bbox: Some(BBox {
                        x0: 10.0,
                        y0: 20.0,
                        x1: 120.0,
                        y1: 36.0,
                    }),
                    ocr: Box::new(OcrLocatorMetadata {
                        profile: OcrProfile {
                            provider: "test".into(),
                            engine: "fixture-ocr".into(),
                            engine_version: Some("1.0".into()),
                            language: "eng".into(),
                            profile: "default".into(),
                        },
                        profile_hash: "ocr-profile-hash".into(),
                        confidence: Some(0.91),
                        text_hash: "ocr-text-hash".into(),
                    }),
                },
                text: "ocrneedle scanned invoice total".into(),
                text_hash: "ocr-text-hash".into(),
                heading_path: vec!["OCR text".into()],
                position: 3,
            }],
            provenance: RetrievalProvenance::seed(1, chunk_id, source_id),
        }]
    }

    fn sample_graphrag_report_results() -> Vec<RetrievalResult> {
        let source_id = SourceId("src".into());
        let report_id = EvidenceId("graphrag:report:community-test".into());
        let source_evidence_id = EvidenceId("ev-text-1".into());
        let chunk_id = ChunkId("graphrag:report-chunk:community-test".into());
        vec![RetrievalResult {
            chunk_id: chunk_id.clone(),
            score: 2.0,
            chunk: Chunk {
                id: chunk_id.clone(),
                source_id: source_id.clone(),
                text: "Community report: Alpha\nGrounded claims:\n- Alpha is grounded.".into(),
                context_text: None,
                token_count: 8,
                chunk_type: ChunkType::Child,
                parent_chunk_id: None,
                heading_path: vec!["GraphRAG global search".into()],
                evidence_unit_ids: vec![report_id.clone(), source_evidence_id.clone()],
            },
            evidence_units: vec![
                EvidenceUnit {
                    id: report_id,
                    source_id: source_id.clone(),
                    kind: EvidenceKind::Generated,
                    derived_from: None,
                    locator: SourceLocator::Document {
                        path_or_url: "graphrag://community/community-test".into(),
                        line_start: 1,
                        line_end: None,
                    },
                    text: "Community report: Alpha\nGrounded claims:\n- Alpha is grounded.".into(),
                    text_hash: "graphrag-report-hash".into(),
                    heading_path: vec!["GraphRAG global search".into()],
                    position: 0,
                },
                EvidenceUnit {
                    id: source_evidence_id,
                    source_id: source_id.clone(),
                    kind: EvidenceKind::Text,
                    derived_from: None,
                    locator: SourceLocator::Document {
                        path_or_url: "doc.md".into(),
                        line_start: 1,
                        line_end: Some(2),
                    },
                    text: "Alpha source text.".into(),
                    text_hash: "source-text-hash".into(),
                    heading_path: vec![],
                    position: 1,
                },
            ],
            provenance: RetrievalProvenance::seed(1, chunk_id, source_id),
        }]
    }

    #[test]
    fn source_pack_includes_all_evidence() {
        let pack = build_source_pack(&sample_results(), &GenerationContext::default(), false);
        assert!(pack.text.contains("[E1 | original_text |"));
        assert!(pack.text.contains("[E2 | original_text |"));
        assert!(pack.text.contains("original_text:\nFreedom is defined"));
        assert_eq!(pack.evidence_refs.len(), 2);
    }

    #[test]
    fn extract_cited_references() {
        let pack = build_source_pack(&sample_results(), &GenerationContext::default(), false);
        let answer = "The concept [E1, E2] is important.";
        let citations = extract_citations(answer, &pack.evidence_refs);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].label, "E1");
        assert_eq!(citations[1].label, "E2");
    }

    #[test]
    fn source_pack_distinguishes_image_caption_and_artifact() {
        let context = GenerationContext::new(vec![sample_image_artifact()], Vec::new());
        let pack = build_source_pack(&sample_image_caption_results(), &context, false);

        assert!(pack.text.contains("[E1 | image_caption_generated |"));
        assert!(pack.text.contains("derived_from=img-1"));
        assert!(pack.text.contains("not original PDF text"));
        assert!(pack.text.contains("[E2 | image_artifact |"));
        assert!(pack
            .text
            .contains("image_artifact_metadata: image_id=img-1"));
        assert!(pack.text.contains("vision_attachment: not_included"));
    }

    #[test]
    fn source_pack_distinguishes_ocr_text_from_generated_text() {
        let pack = build_source_pack(&sample_ocr_results(), &GenerationContext::default(), false);

        assert!(pack.text.contains("[E1 | ocr_text |"));
        assert!(pack
            .text
            .contains("ocr_text:\nocrneedle scanned invoice total"));
        assert!(pack.text.contains("OCR-derived source text"));
        assert!(!pack.text.contains("generated_text:\n"));

        let citations = extract_citations("The scan shows a total [E1].", &pack.evidence_refs);
        let value = serde_json::to_value(verifier_source_inputs(&citations, &[])).unwrap();
        assert_eq!(value[0]["kind"], "ocr_text");
        assert_eq!(
            value[0]["provenance"]["summary"],
            "OCR-derived source text with structured locator"
        );
        assert_eq!(value[0]["visual_support"]["support_level"], "ocr_text");
    }

    #[test]
    fn graphrag_report_is_generic_generated_text() {
        let results = sample_graphrag_report_results();
        let pack = build_source_pack(&results, &GenerationContext::default(), true);

        assert!(pack.text.contains("[E1 | generated |"));
        assert!(pack
            .text
            .contains("generated_text:\nCommunity report: Alpha"));
        assert!(!pack.text.contains("[E1 | image_caption_generated |"));
        assert!(!pack.text.contains("derived_from=ev-text-1"));
        assert!(!pack.text.contains("vision_attachment:"));
        assert!(selected_image_evidence_ids(&results).is_empty());

        let citations = extract_citations("Alpha is grounded [E1].", &pack.evidence_refs);
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].kind, EvidenceKind::Generated);
        assert_eq!(citations[0].derived_from, None);

        let inputs = verifier_source_inputs(&citations, &[]);
        let value = serde_json::to_value(&inputs).unwrap();
        assert_eq!(value[0]["kind"], "generated");
        assert_eq!(
            value[0]["provenance"]["summary"],
            "generated derived evidence"
        );
        assert!(value[0]["provenance"]["derived_from"].is_null());
        assert_eq!(
            value[0]["visual_support"]["support_level"],
            "generated_text"
        );
        assert_eq!(
            value[0]["visual_support"]["vision_attachment"],
            "not_applicable"
        );
        assert!(value[0]["visual_support"]["image_evidence_id"].is_null());
    }

    #[test]
    fn verifier_schema_includes_kind_derivation_and_visual_support() {
        let mut results = sample_results();
        results.extend(sample_image_caption_results());
        let context = GenerationContext::new(
            vec![sample_image_artifact()],
            vec![ImageAttachment {
                evidence_id: EvidenceId("img-1".into()),
                mime_type: "image/png".into(),
                bytes: b"abc".to_vec(),
            }],
        );
        let pack = build_source_pack(&results, &context, true);
        let citations = extract_citations(
            "Freedom is textual [E1]. The caption mentions chartneedle [E3]. The image is cited [E4].",
            &pack.evidence_refs,
        );

        let inputs = verifier_source_inputs(&citations, &pack.attachments);
        let value = serde_json::to_value(&inputs).unwrap();

        assert_eq!(value[0]["kind"], "original_text");
        assert_eq!(value[0]["source_id"], "src");
        assert_eq!(
            value[0]["provenance"]["derivation_chain"][0]["kind"],
            "original_text"
        );
        assert_eq!(value[0]["visual_support"]["support_level"], "text_only");

        assert_eq!(value[1]["kind"], "image_caption_generated");
        assert_eq!(value[1]["provenance"]["derived_from"], "img-1");
        assert_eq!(
            value[1]["provenance"]["derivation_chain"][0]["kind"],
            "image_artifact"
        );
        assert_eq!(
            value[1]["provenance"]["derivation_chain"][0]["source_id"],
            "src"
        );
        assert_eq!(
            value[1]["provenance"]["derivation_chain"][1]["kind"],
            "image_caption_generated"
        );
        assert_eq!(
            value[1]["visual_support"]["support_level"],
            "caption_plus_pixels"
        );
        assert_eq!(value[1]["visual_support"]["vision_attachment"], "included");

        assert_eq!(value[2]["kind"], "image_artifact");
        assert_eq!(value[2]["locator"]["structured"]["type"], "PdfImage");
        assert_eq!(
            value[2]["visual_support"]["support_level"],
            "image_pixels_available"
        );
        assert_eq!(value[2]["visual_support"]["image_evidence_id"], "img-1");
    }

    #[test]
    fn verifier_schema_marks_caption_only_support_conservative() {
        let context = GenerationContext::new(vec![sample_image_artifact()], Vec::new());
        let pack = build_source_pack(&sample_image_caption_results(), &context, false);
        let citations = extract_citations(
            "The generated caption mentions chartneedle [E1].",
            &pack.evidence_refs,
        );

        let inputs = verifier_source_inputs(&citations, &[]);
        let value = serde_json::to_value(&inputs).unwrap();

        assert_eq!(value[0]["kind"], "image_caption_generated");
        assert_eq!(value[0]["provenance"]["derived_from"], "img-1");
        assert_eq!(
            value[0]["visual_support"]["support_level"],
            "caption_only_conservative"
        );
        assert_eq!(
            value[0]["visual_support"]["vision_attachment"],
            "not_included"
        );
        assert!(value[0]["visual_support"]["caution"]
            .as_str()
            .unwrap()
            .contains("weaker than original text"));
    }

    #[test]
    fn render_appends_references() {
        let citations = vec![CitationRef {
            label: "E1".into(),
            evidence_id: EvidenceId("ev-1".into()),
            source_id: SourceId("src".into()),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Pdf {
                page: 42,
                paragraph: 3,
                bbox: None,
            },
            text_preview: "Freedom...".into(),
        }];
        let rendered = render_answer("Answer text [E1].", &citations);
        assert!(rendered.contains("References:"));
        assert!(rendered.contains("[E1] original_text: PDF p.42"));
    }

    #[tokio::test]
    async fn streaming_generation_forwards_deltas_and_extracts_citations() {
        let model = Arc::new(StreamingChatModel::new([
            "The document ",
            "defines freedom [E1].",
        ]));
        let generator =
            Generator::with_chat_model(model.clone(), true, ChatVisionAttachmentConfig::default());
        let mut deltas = Vec::new();

        let result = generator
            .generate_streaming_with_context(
                "What is freedom?",
                &sample_results(),
                &GenerationContext::default(),
                |delta| {
                    deltas.push(delta.to_string());
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(deltas, vec!["The document ", "defines freedom [E1]."]);
        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.citations[0].label, "E1");
        assert!(result.answer.contains("References:"));
        assert!(!result.verified);
        assert_eq!(model.requests().len(), 1);
    }

    #[tokio::test]
    async fn streaming_generation_propagates_delta_callback_errors() {
        let model = Arc::new(StreamingChatModel::new(["first ", "second [E1]."]));
        let generator =
            Generator::with_chat_model(model.clone(), true, ChatVisionAttachmentConfig::default());

        let result = generator
            .generate_streaming_with_context(
                "What is freedom?",
                &sample_results(),
                &GenerationContext::default(),
                |_delta| anyhow::bail!("token sink backpressure"),
            )
            .await;
        let err = match result {
            Ok(_) => panic!("streaming generation should stop on delta callback errors"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("token sink backpressure"));
        assert_eq!(model.requests().len(), 1);
    }

    #[tokio::test]
    async fn mvp_regression_verifier_pass_revise_and_fail_paths_are_deterministic() {
        let pass_model = Arc::new(RecordingChatModel::with_responses([
            "The document defines freedom [E1].",
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ]));
        let pass_generator =
            Generator::with_chat_model(pass_model, true, ChatVisionAttachmentConfig::default());

        let pass = pass_generator
            .generate("What is defined?", &sample_results())
            .await
            .unwrap();

        assert!(pass.verified);
        assert_eq!(pass.citations.len(), 1);
        assert!(pass.answer.contains("defines freedom"));

        let revise_model = Arc::new(RecordingChatModel::with_responses([
            "The page shows a blue triangle [E1].",
            r#"{"verdict":"revise","unsupported_claims":["The page shows a blue triangle"]}"#,
            "The document defines freedom [E1].",
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ]));
        let revise_generator =
            Generator::with_chat_model(revise_model, true, ChatVisionAttachmentConfig::default());

        let revised = revise_generator
            .generate("What does the page show?", &sample_results())
            .await
            .unwrap();

        assert!(revised.verified);
        assert_eq!(revised.citations.len(), 1);
        assert!(revised.answer.contains("defines freedom"));
        assert!(!revised.answer.contains("blue triangle"));

        let fail_model = Arc::new(RecordingChatModel::with_responses([
            "The page shows a blue triangle [E1].",
            r#"{"verdict":"fail","unsupported_claims":["The page shows a blue triangle"]}"#,
        ]));
        let fail_generator =
            Generator::with_chat_model(fail_model, true, ChatVisionAttachmentConfig::default());

        let failed = fail_generator
            .generate("What does the page show?", &sample_results())
            .await
            .unwrap();

        assert_eq!(
            failed.answer,
            "Evidence insufficient to answer this question."
        );
        assert!(failed.citations.is_empty());
        assert!(!failed.verified);
    }

    #[tokio::test]
    async fn unsupported_visual_claim_fails_without_visual_or_caption_evidence() {
        let model = Arc::new(RecordingChatModel::with_responses([
            "The page shows a blue triangle [E1].",
            r#"{"verdict":"fail","unsupported_claims":["The page shows a blue triangle"]}"#,
        ]));
        let generator =
            Generator::with_chat_model(model, true, ChatVisionAttachmentConfig::default());

        let result = generator
            .generate("What does the page show?", &sample_results())
            .await
            .unwrap();

        assert_eq!(
            result.answer,
            "Evidence insufficient to answer this question."
        );
        assert!(result.citations.is_empty());
        assert!(!result.verified);
    }

    #[tokio::test]
    async fn unsupported_visual_claim_is_revised_before_return() {
        let model = Arc::new(RecordingChatModel::with_responses([
            "The page shows a blue triangle [E1].",
            r#"{"verdict":"revise","unsupported_claims":["The page shows a blue triangle"]}"#,
            "The document defines freedom [E1].",
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ]));
        let generator =
            Generator::with_chat_model(model.clone(), true, ChatVisionAttachmentConfig::default());

        let result = generator
            .generate("What does the page show?", &sample_results())
            .await
            .unwrap();

        assert!(result.verified);
        assert!(result.answer.contains("defines freedom"));
        assert!(!result.answer.contains("blue triangle"));
        assert_eq!(result.citations.len(), 1);
        assert_eq!(model.requests().len(), 4);
    }

    #[tokio::test]
    async fn supported_caption_claim_passes_with_caption_provenance_visible() {
        let model = Arc::new(RecordingChatModel::with_responses([
            "The generated caption says chartneedle appears in the diagram [E1].",
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ]));
        let generator = Generator::with_chat_model(
            model.clone(),
            true,
            ChatVisionAttachmentConfig {
                enabled: false,
                model_supports_vision: true,
                ..ChatVisionAttachmentConfig::default()
            },
        );
        let context = GenerationContext::new(vec![sample_image_artifact()], Vec::new());

        let result = generator
            .generate_with_context(
                "What does the generated caption say?",
                &sample_image_caption_results(),
                &context,
            )
            .await
            .unwrap();

        assert!(result.verified);
        assert!(result.answer.contains("generated caption says"));
        assert!(result.answer.contains("[E1] image_caption_generated"));
        assert!(result.answer.contains("derived from img-1"));

        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        assert_request_is_text_only(&requests[1]);
        match &requests[1].messages[1].content {
            ChatMessageContent::Text(text) => {
                assert!(text.contains("generated image caption derived"));
                assert!(text.contains("\"derived_from\": \"img-1\""));
                assert!(text.contains("caption_only_conservative"));
            }
            ChatMessageContent::Parts(_) => {
                panic!("disabled attachments must keep verifier text-only")
            }
        }
    }

    #[tokio::test]
    async fn image_caption_generation_with_attachments_disabled_sends_text_only() {
        let model = Arc::new(RecordingChatModel::new(
            "The diagram mentions chartneedle [E1].",
        ));
        let generator = Generator::with_chat_model(
            model.clone(),
            false,
            ChatVisionAttachmentConfig {
                enabled: false,
                model_supports_vision: true,
                ..ChatVisionAttachmentConfig::default()
            },
        );
        let context = GenerationContext::new(
            vec![sample_image_artifact()],
            vec![ImageAttachment {
                evidence_id: EvidenceId("img-1".into()),
                mime_type: "image/png".into(),
                bytes: b"abc".to_vec(),
            }],
        );

        let result = generator
            .generate_with_context(
                "What does the diagram show?",
                &sample_image_caption_results(),
                &context,
            )
            .await
            .unwrap();

        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.citations[0].label, "E1");
        assert_eq!(result.citations[0].kind, EvidenceKind::Generated);
        assert_eq!(
            result.citations[0].derived_from,
            Some(EvidenceId("img-1".into()))
        );

        let requests = model.requests();
        match &requests[0].messages[1].content {
            ChatMessageContent::Text(text) => {
                assert!(text.contains("image_caption_generated"));
                assert!(text.contains("vision_attachment: not_included"));
            }
            ChatMessageContent::Parts(_) => {
                panic!("disabled attachments must not send image parts")
            }
        }
    }

    #[tokio::test]
    async fn image_caption_generation_with_enabled_budget_sends_image_part() {
        let model = Arc::new(RecordingChatModel::new(
            "The attached image and caption support chartneedle [E1, E2].",
        ));
        let generator = Generator::with_chat_model(
            model.clone(),
            false,
            ChatVisionAttachmentConfig {
                enabled: true,
                model_supports_vision: true,
                max_images: 1,
                max_total_bytes: 32,
                detail: "low".into(),
            },
        );
        let context = GenerationContext::new(
            vec![sample_image_artifact()],
            vec![ImageAttachment {
                evidence_id: EvidenceId("img-1".into()),
                mime_type: "image/png".into(),
                bytes: b"abc".to_vec(),
            }],
        );

        let result = generator
            .generate_with_context(
                "What does the diagram show?",
                &sample_image_caption_results(),
                &context,
            )
            .await
            .unwrap();

        assert_eq!(result.citations.len(), 2);
        assert_eq!(result.citations[0].label, "E1");
        assert_eq!(result.citations[1].label, "E2");

        let requests = model.requests();
        match &requests[0].messages[1].content {
            ChatMessageContent::Parts(parts) => {
                assert!(matches!(parts[0], ChatContentPart::Text { .. }));
                assert!(parts.iter().any(|part| {
                    matches!(
                        part,
                        ChatContentPart::Text { text }
                            if text.contains("Attached image evidence [E1], [E2]")
                    )
                }));
                assert!(parts.iter().any(|part| {
                    matches!(
                        part,
                        ChatContentPart::ImageUrl { image_url }
                            if image_url.url == "data:image/png;base64,YWJj"
                                && image_url.detail.as_deref() == Some("low")
                    )
                }));
            }
            ChatMessageContent::Text(_) => panic!("enabled attachments should send image parts"),
        }
    }

    #[tokio::test]
    async fn verifier_and_revision_receive_cited_image_attachments() {
        let model = Arc::new(RecordingChatModel::with_responses([
            "The image shows a blue triangle [E2].",
            r#"{"verdict":"revise","unsupported_claims":["tighten wording"]}"#,
            "The attached image shows a blue triangle [E2].",
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ]));
        let generator = Generator::with_chat_model(
            model.clone(),
            true,
            ChatVisionAttachmentConfig {
                enabled: true,
                model_supports_vision: true,
                max_images: 1,
                max_total_bytes: 32,
                detail: "low".into(),
            },
        );
        let context = GenerationContext::new(
            vec![sample_image_artifact()],
            vec![ImageAttachment {
                evidence_id: EvidenceId("img-1".into()),
                mime_type: "image/png".into(),
                bytes: b"abc".to_vec(),
            }],
        );

        let result = generator
            .generate_with_context(
                "What shape is visible in the image?",
                &sample_image_caption_results(),
                &context,
            )
            .await
            .unwrap();

        assert!(result.verified);
        assert!(result.answer.contains("blue triangle"));
        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.citations[0].label, "E2");
        assert!(!result.citations[0].text_preview.contains("blue triangle"));

        let requests = model.requests();
        assert_eq!(requests.len(), 4);
        assert_request_has_image_part(&requests[0], "Attached image evidence [E1], [E2]");
        assert_request_has_image_part(&requests[1], "Attached image evidence [E2]");
        assert_request_has_image_part(&requests[2], "Attached image evidence [E2]");
        assert_request_has_image_part(&requests[3], "Attached image evidence [E2]");
    }

    #[tokio::test]
    async fn verifier_stays_text_only_when_vision_attachments_disabled_or_unsupported() {
        let configs = [
            ChatVisionAttachmentConfig {
                enabled: false,
                model_supports_vision: true,
                ..ChatVisionAttachmentConfig::default()
            },
            ChatVisionAttachmentConfig {
                enabled: true,
                model_supports_vision: false,
                ..ChatVisionAttachmentConfig::default()
            },
        ];

        for config in configs {
            let model = Arc::new(RecordingChatModel::with_responses([
                "The generated caption mentions chartneedle [E1].",
                r#"{"verdict":"pass","unsupported_claims":[]}"#,
            ]));
            let generator = Generator::with_chat_model(model.clone(), true, config);
            let context = GenerationContext::new(
                vec![sample_image_artifact()],
                vec![ImageAttachment {
                    evidence_id: EvidenceId("img-1".into()),
                    mime_type: "image/png".into(),
                    bytes: b"abc".to_vec(),
                }],
            );

            let result = generator
                .generate_with_context(
                    "What does the generated caption say?",
                    &sample_image_caption_results(),
                    &context,
                )
                .await
                .unwrap();

            assert!(result.verified);
            let requests = model.requests();
            assert_eq!(requests.len(), 2);
            for request in &requests {
                assert_request_is_text_only(request);
            }
        }
    }

    #[test]
    fn image_attachment_selection_respects_max_images_budget() {
        let mut artifact_two = sample_image_artifact();
        artifact_two.image_id = ImageId("img-2".into());
        artifact_two.evidence_id = EvidenceId("img-2".into());
        artifact_two.image_index = 3;

        let mut results = sample_image_caption_results();
        results[0].evidence_units.push(EvidenceUnit {
            id: EvidenceId("img-2".into()),
            source_id: SourceId("src".into()),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::PdfImage {
                page: 8,
                image_index: 3,
                bbox: None,
            },
            text: "Second image evidence.".into(),
            text_hash: "image-hash-2".into(),
            heading_path: vec![],
            position: 11,
        });

        let attachments = select_image_attachments(
            &results,
            &[sample_image_artifact(), artifact_two],
            &ChatVisionAttachmentConfig {
                enabled: true,
                model_supports_vision: true,
                max_images: 1,
                max_total_bytes: 32,
                detail: "auto".into(),
            },
            |artifact| Ok(format!("bytes-{}", artifact.evidence_id.0.as_str()).into_bytes()),
        )
        .unwrap();

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].evidence_id, EvidenceId("img-1".into()));
    }

    #[test]
    fn insufficient_generation_does_not_expose_citations() {
        let result = insufficient_generation();
        assert_eq!(
            result.answer,
            "Evidence insufficient to answer this question."
        );
        assert!(result.citations.is_empty());
        assert!(!result.verified);
    }
}
