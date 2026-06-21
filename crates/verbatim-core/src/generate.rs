use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{ChatConfig, VerifierConfig};
use crate::provider::openai_compatible::OpenAiCompatibleChatModel;
use crate::provider::{ChatMessage, ChatModel, ChatRequest};
use crate::types::{CitationRef, EvidenceUnit, RetrievalResult};

pub struct Generator {
    chat_model: OpenAiCompatibleChatModel,
    verifier_enabled: bool,
}

pub struct GenerationResult {
    pub answer: String,
    pub citations: Vec<CitationRef>,
    pub verified: bool,
}

impl Generator {
    pub fn new(chat: &ChatConfig, verifier: &VerifierConfig) -> Self {
        Self {
            chat_model: OpenAiCompatibleChatModel::from_config(chat),
            verifier_enabled: verifier.enabled,
        }
    }

    pub async fn generate(
        &self,
        question: &str,
        results: &[RetrievalResult],
    ) -> Result<GenerationResult> {
        let (source_pack, eid_map) = build_source_pack(results);

        let system_prompt = SYSTEM_PROMPT;
        let user_prompt = format!("SOURCE PACK:\n{source_pack}\n\nUSER QUESTION:\n{question}");

        let raw_answer = self.chat(system_prompt, &user_prompt).await?;

        let citations = extract_citations(&raw_answer, &eid_map);
        if self.verifier_enabled {
            let verification = self.verify(question, &raw_answer, &citations).await?;
            return self
                .apply_verification(question, &raw_answer, citations, verification)
                .await;
        }

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
        let sources_json: Vec<serde_json::Value> = citations
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.evidence_id.0,
                    "locator": c.locator.to_string(),
                    "text": c.text_preview,
                })
            })
            .collect();

        let prompt = format!(
            "Verify this answer against the cited sources.\n\n\
             Question: {question}\n\n\
             Answer: {answer}\n\n\
             Sources: {sources}\n\n\
             Output JSON with this schema:\n\
             {{\"verdict\": \"pass|revise|fail\", \
             \"unsupported_claims\": [\"claim text\"]}}",
            sources = serde_json::to_string_pretty(&sources_json)?
        );

        let response = self
            .chat(
                "You are a citation verification system. Output only valid JSON.",
                &prompt,
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
        verification: VerificationResult,
    ) -> Result<GenerationResult> {
        match verification.verdict.as_str() {
            "pass" => Ok(GenerationResult {
                answer: render_answer(raw_answer, &citations),
                citations,
                verified: true,
            }),
            "revise" => {
                let revised = self
                    .revise_answer(
                        question,
                        raw_answer,
                        &citations,
                        &verification.unsupported_claims,
                    )
                    .await?;
                let second_pass = self.verify(question, &revised, &citations).await?;
                if second_pass.verdict == "pass" {
                    Ok(GenerationResult {
                        answer: render_answer(&revised, &citations),
                        citations,
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
        unsupported_claims: &[String],
    ) -> Result<String> {
        let sources_json: Vec<serde_json::Value> = citations
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.evidence_id.0,
                    "locator": c.locator.to_string(),
                    "text": c.text_preview,
                })
            })
            .collect();
        let prompt = format!(
            "Revise the answer so every remaining factual claim is directly supported by the cited sources.\n\n\
             Question: {question}\n\n\
             Original answer: {answer}\n\n\
             Unsupported claims to remove: {unsupported}\n\n\
             Sources: {sources}\n\n\
             If the sources are insufficient, output exactly: Evidence insufficient to answer this question.",
            unsupported = serde_json::to_string_pretty(unsupported_claims)?,
            sources = serde_json::to_string_pretty(&sources_json)?
        );
        self.chat(
            "You revise answers to remove unsupported claims. Output only the revised answer.",
            &prompt,
        )
        .await
    }

    async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let response = self
            .chat_model
            .chat(ChatRequest::new(vec![
                ChatMessage::system(system),
                ChatMessage::user(user),
            ]))
            .await
            .context("chat completion failed")?;
        Ok(response.content)
    }
}

fn insufficient_generation() -> GenerationResult {
    GenerationResult {
        answer: "Evidence insufficient to answer this question.".into(),
        citations: Vec::new(),
        verified: false,
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationResult {
    pub verdict: String,
    pub unsupported_claims: Vec<String>,
}

fn build_source_pack(results: &[RetrievalResult]) -> (String, HashMap<String, EvidenceRef>) {
    let mut pack = String::new();
    let mut eid_map: HashMap<String, EvidenceRef> = HashMap::new();
    let mut counter = 1;

    let mut seen_evidence: HashMap<String, usize> = HashMap::new();

    for result in results {
        for eu in &result.evidence_units {
            if seen_evidence.contains_key(&eu.id.0) {
                continue;
            }

            let eid_label = format!("E{counter}");
            seen_evidence.insert(eu.id.0.clone(), counter);

            pack.push_str(&format!(
                "[{eid_label} | {locator}]\n{text}\n\n",
                locator = eu.locator,
                text = eu.text
            ));

            eid_map.insert(
                eid_label,
                EvidenceRef {
                    evidence: eu.clone(),
                },
            );

            counter += 1;
        }
    }

    (pack, eid_map)
}

struct EvidenceRef {
    evidence: EvidenceUnit,
}

fn extract_citations(answer: &str, eid_map: &HashMap<String, EvidenceRef>) -> Vec<CitationRef> {
    let mut citations = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (label, eref) in eid_map {
        let patterns = [
            format!("[{label}]"),
            format!("[{label},"),
            format!("[{label} "),
        ];
        let found = patterns.iter().any(|p| answer.contains(p.as_str()));
        if found && seen.insert(label.clone()) {
            citations.push(CitationRef {
                evidence_id: eref.evidence.id.clone(),
                locator: eref.evidence.locator.clone(),
                text_preview: eref.evidence.text.chars().take(200).collect(),
            });
        }
    }

    citations.sort_by_key(|c| c.evidence_id.0.clone());
    citations
}

fn render_answer(raw: &str, citations: &[CitationRef]) -> String {
    let mut output = raw.to_string();

    output.push_str("\n\nReferences:\n");
    for (i, cite) in citations.iter().enumerate() {
        output.push_str(&format!("[{}] {}\n", i + 1, cite.locator));
    }

    output
}

const SYSTEM_PROMPT: &str = "\
You are answering questions about documents.

Rules:
1. Use ONLY the provided SOURCE PACK.
2. Every factual claim must cite one or more source ids like [E1].
3. Do not cite sources that do not directly support the sentence.
4. If the SOURCE PACK does not contain enough evidence, say so.
5. Do not use outside knowledge.
6. Do not invent page numbers, paragraph numbers, quotations, or citations.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Chunk, ChunkId, ChunkType, EvidenceId, SourceId, SourceLocator};

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
        }]
    }

    #[test]
    fn source_pack_includes_all_evidence() {
        let (pack, map) = build_source_pack(&sample_results());
        assert!(pack.contains("[E1 |"));
        assert!(pack.contains("[E2 |"));
        assert!(pack.contains("Freedom is defined"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn extract_cited_references() {
        let (_, map) = build_source_pack(&sample_results());
        let answer = "The concept [E1] is important. As noted [E2], the argument holds.";
        let citations = extract_citations(answer, &map);
        assert_eq!(citations.len(), 2);
    }

    #[test]
    fn render_appends_references() {
        let citations = vec![CitationRef {
            evidence_id: EvidenceId("ev-1".into()),
            locator: SourceLocator::Pdf {
                page: 42,
                paragraph: 3,
                bbox: None,
            },
            text_preview: "Freedom...".into(),
        }];
        let rendered = render_answer("Answer text [E1].", &citations);
        assert!(rendered.contains("References:"));
        assert!(rendered.contains("[1] PDF p.42"));
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
