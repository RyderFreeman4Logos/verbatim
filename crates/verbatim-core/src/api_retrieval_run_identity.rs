use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::{
    AuditReceipt, CollectionFilterResponse, OutputTextPlane, RetrieveControlsResponse,
    RetrieveResponse, RetrieveResultResponse, TextFieldTaxonomy,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
pub(super) struct RetrievalRunIdentityBody {
    pub(super) task_id: String,
    pub(super) query: String,
    pub(super) source_id: Option<String>,
    pub(super) collection_filter: Option<CollectionFilterResponse>,
    pub(super) embedding_profile_id: String,
    pub(super) generation: Option<String>,
    pub(super) limit: usize,
    pub(super) page_size: usize,
    pub(super) page: usize,
    pub(super) total_results: usize,
    pub(super) returned_results: usize,
    pub(super) source_bounded: bool,
    pub(super) controls: RetrieveControlsResponse,
    pub(super) audit_receipt: AuditReceipt,
    pub(super) results: Vec<RetrieveResultResponse>,
}

impl RetrievalRunIdentityBody {
    pub(super) fn from_response(response: &RetrieveResponse) -> Self {
        Self {
            task_id: response.task_id.clone(),
            query: response.query.clone(),
            source_id: response.source_id.clone(),
            collection_filter: response.collection_filter.clone(),
            embedding_profile_id: response.embedding_profile_id.clone(),
            generation: response.generation.clone(),
            limit: response.limit,
            page_size: response.page_size,
            page: response.page,
            total_results: response.total_results,
            returned_results: response.returned_results,
            source_bounded: response.source_bounded,
            controls: response.controls.clone(),
            audit_receipt: response.audit_receipt.clone(),
            results: response.results.clone(),
        }
    }
}

pub(super) fn stamp_retrieval_run_identity(
    body: &RetrievalRunIdentityBody,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::RetrievalRun,
        WIRE_SCHEMA_VERSION,
        body.task_id.clone(),
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("retrieval-run identity does not match the executed response body");
        }
    }
    Ok(expected)
}

const RETRIEVAL_RUN_IDENTITY_FIELDS: &[&str] = &[
    "identity.artifact_id",
    "identity.content_hash",
    "identity.kind",
];

pub(super) struct RetrievalRunIdentityTaxonomy {
    synthesize: bool,
    emitted: bool,
}

impl RetrievalRunIdentityTaxonomy {
    pub(super) fn start(object: &serde_json::Map<String, Value>) -> Self {
        Self {
            synthesize: !object.contains_key("identity")
                && object.contains_key("task_id")
                && object.contains_key("query")
                && object.contains_key("results"),
            emitted: false,
        }
    }

    pub(super) fn emit_before(
        &mut self,
        path: &str,
        key: &str,
        fields: &mut Vec<TextFieldTaxonomy>,
    ) {
        if self.synthesize && !self.emitted && key > "identity" {
            self.push_fields(path, fields);
            self.emitted = true;
        }
    }

    pub(super) fn emit_after(&mut self, path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
        if self.synthesize && !self.emitted {
            self.push_fields(path, fields);
            self.emitted = true;
        }
    }

    fn push_fields(&self, path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}.")
        };
        for field in RETRIEVAL_RUN_IDENTITY_FIELDS {
            fields.push(TextFieldTaxonomy {
                field: format!("{prefix}{field}"),
                plane: OutputTextPlane::Metadata,
            });
        }
    }
}
