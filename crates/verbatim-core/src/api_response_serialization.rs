use serde::{Deserialize, Serialize};

use super::{
    retrieve_envelope, AnswerKind, AskResponse, AuditReceipt, CitationResponse,
    CollectionFilterResponse, EvidenceResponse, GeneratedInterpretationResponse,
    ImageArtifactResponse, ResponseTextTaxonomy, RetrievalDebug, RetrieveControlsResponse,
    RetrieveResponse, RetrieveResultResponse, RetrieveTimingResponse, SourceLocator,
};
use crate::wire_schemas::{ContextPackEnvelope, EvidencePackEnvelope};

#[derive(Debug, Serialize, Deserialize)]
struct AskResponseWire {
    answer: String,
    answer_kind: AnswerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_taxonomy: Option<ResponseTextTaxonomy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated_interpretation: Option<GeneratedInterpretationResponse>,
    #[serde(default)]
    citations: Vec<CitationResponse>,
    verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retrieval: Option<RetrievalDebug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<RetrieveResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_pack: Option<ContextPackEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collection_filter: Option<CollectionFilterResponse>,
}

impl Serialize for AskResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = AskResponseWire {
            answer: self.answer.clone(),
            answer_kind: self.answer_kind,
            text_taxonomy: None,
            generated_interpretation: self.generated_interpretation.clone(),
            citations: self.citations.clone(),
            verified: self.verified,
            retrieval: self.retrieval.clone(),
            context: match self.answer_kind {
                AnswerKind::GeneratedInterpretation => None,
                AnswerKind::EvidenceOnly => self.context.clone(),
            },
            context_pack: retrieve_envelope::context_pack_from_ask_context(self.context.as_ref())
                .map_err(serde::ser::Error::custom)?,
            collection_filter: self.collection_filter.clone(),
        };
        let mut value = serde_json::to_value(wire).map_err(serde::ser::Error::custom)?;
        let taxonomy = serde_json::to_value(ResponseTextTaxonomy::from_serialized_value(&value))
            .map_err(serde::ser::Error::custom)?;
        let Some(object) = value.as_object_mut() else {
            return Err(serde::ser::Error::custom(
                "ask response wire is not an object",
            ));
        };
        object.insert("text_taxonomy".into(), taxonomy);
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskResponseWire::deserialize(deserializer)?;
        retrieve_envelope::bind_context_pack_to_ask_context(
            wire.context.as_ref(),
            wire.context_pack.as_ref(),
        )
        .map_err(serde::de::Error::custom)?;
        let value = serde_json::to_value(&wire).map_err(serde::de::Error::custom)?;
        let text_taxonomy = ResponseTextTaxonomy::from_serialized_value(&value);
        Ok(Self {
            answer: wire.answer,
            answer_kind: wire.answer_kind,
            text_taxonomy,
            generated_interpretation: wire.generated_interpretation,
            citations: wire.citations,
            verified: wire.verified,
            retrieval: wire.retrieval,
            context: wire.context,
            collection_filter: wire.collection_filter,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RetrieveResponseWire {
    task_id: String,
    query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_taxonomy: Option<ResponseTextTaxonomy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collection_filter: Option<CollectionFilterResponse>,
    embedding_profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
    limit: usize,
    page_size: usize,
    page: usize,
    total_results: usize,
    returned_results: usize,
    source_bounded: bool,
    controls: RetrieveControlsResponse,
    audit_receipt: AuditReceipt,
    #[serde(default)]
    timings: Vec<RetrieveTimingResponse>,
    #[serde(default)]
    results: Vec<RetrieveResultResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    debug: Option<RetrievalDebug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_pack: Option<EvidencePackEnvelope>,
}

impl Serialize for RetrieveResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = RetrieveResponseWire {
            task_id: self.task_id.clone(),
            query: self.query.clone(),
            text_taxonomy: None,
            source_id: self.source_id.clone(),
            collection_filter: self.collection_filter.clone(),
            embedding_profile_id: self.embedding_profile_id.clone(),
            generation: self.generation.clone(),
            limit: self.limit,
            page_size: self.page_size,
            page: self.page,
            total_results: self.total_results,
            returned_results: self.returned_results,
            source_bounded: self.source_bounded,
            controls: self.controls.clone(),
            audit_receipt: self.audit_receipt.clone(),
            timings: self.timings.clone(),
            results: self.results.clone(),
            debug: self.debug.clone(),
            evidence_pack: retrieve_envelope::evidence_pack_from_retrieve(
                &self.query,
                &self.results,
                &self.embedding_profile_id,
                self.generation.as_deref(),
            )
            .map_err(serde::ser::Error::custom)?,
        };
        let mut value = serde_json::to_value(wire).map_err(serde::ser::Error::custom)?;
        let taxonomy = serde_json::to_value(ResponseTextTaxonomy::from_serialized_value(&value))
            .map_err(serde::ser::Error::custom)?;
        let Some(object) = value.as_object_mut() else {
            return Err(serde::ser::Error::custom(
                "retrieve response wire is not an object",
            ));
        };
        object.insert("text_taxonomy".into(), taxonomy);
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RetrieveResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RetrieveResponseWire::deserialize(deserializer)?;
        retrieve_envelope::evidence_pack_from_retrieve(
            &wire.query,
            &wire.results,
            &wire.embedding_profile_id,
            wire.generation.as_deref(),
        )
        .map_err(serde::de::Error::custom)?;
        if let Some(pack) = &wire.evidence_pack {
            retrieve_envelope::bind_evidence_pack_to_retrieve(
                &wire.query,
                &wire.results,
                &wire.embedding_profile_id,
                wire.generation.as_deref(),
                pack,
            )
            .map_err(serde::de::Error::custom)?;
        }
        let value = serde_json::to_value(&wire).map_err(serde::de::Error::custom)?;
        let text_taxonomy = ResponseTextTaxonomy::from_serialized_value(&value);
        Ok(Self {
            task_id: wire.task_id,
            query: wire.query,
            text_taxonomy,
            source_id: wire.source_id,
            collection_filter: wire.collection_filter,
            embedding_profile_id: wire.embedding_profile_id,
            generation: wire.generation,
            limit: wire.limit,
            page_size: wire.page_size,
            page: wire.page,
            total_results: wire.total_results,
            returned_results: wire.returned_results,
            source_bounded: wire.source_bounded,
            controls: wire.controls,
            audit_receipt: wire.audit_receipt,
            timings: wire.timings,
            results: wire.results,
            debug: wire.debug,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct EvidenceResponseWire {
    id: String,
    source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_taxonomy: Option<ResponseTextTaxonomy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_hash: Option<String>,
    source_bounded: bool,
    text_hash: String,
    kind: String,
    #[serde(default)]
    derived_from: Option<String>,
    locator: String,
    structured_locator: SourceLocator,
    text: String,
    heading_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    position: u32,
    #[serde(default)]
    image_artifact: Option<ImageArtifactResponse>,
}

impl<'de> Deserialize<'de> for EvidenceResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let response = EvidenceResponseWire::deserialize(deserializer)?;
        let value = serde_json::to_value(&response).map_err(serde::de::Error::custom)?;
        let text_taxonomy = ResponseTextTaxonomy::from_serialized_value(&value);

        Ok(Self {
            id: response.id,
            source_id: response.source_id,
            text_taxonomy,
            source_hash: response.source_hash,
            source_bounded: response.source_bounded,
            text_hash: response.text_hash,
            kind: response.kind,
            derived_from: response.derived_from,
            locator: response.locator,
            structured_locator: response.structured_locator,
            text: response.text,
            heading_path: response.heading_path,
            language: response.language,
            position: response.position,
            image_artifact: response.image_artifact,
        })
    }
}

impl Serialize for EvidenceResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = EvidenceResponseWire {
            id: self.id.clone(),
            source_id: self.source_id.clone(),
            text_taxonomy: None,
            source_hash: self.source_hash.clone(),
            source_bounded: self.source_bounded,
            text_hash: self.text_hash.clone(),
            kind: self.kind.clone(),
            derived_from: self.derived_from.clone(),
            locator: self.locator.clone(),
            structured_locator: self.structured_locator.clone(),
            text: self.text.clone(),
            heading_path: self.heading_path.clone(),
            language: self.language.clone(),
            position: self.position,
            image_artifact: self.image_artifact.clone(),
        };
        let mut value = serde_json::to_value(wire).map_err(serde::ser::Error::custom)?;
        let taxonomy = serde_json::to_value(ResponseTextTaxonomy::from_serialized_value(&value))
            .map_err(serde::ser::Error::custom)?;
        let Some(object) = value.as_object_mut() else {
            return Err(serde::ser::Error::custom(
                "evidence response wire is not an object",
            ));
        };
        object.insert("text_taxonomy".into(), taxonomy);
        value.serialize(serializer)
    }
}
