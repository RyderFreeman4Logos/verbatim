use anyhow::Result;
use serde::Serialize;

use super::{AnswerKind, CitationResponse, CollectionFilterResponse};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, ContextPackEnvelope, WireArtifactKind,
    WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
pub(super) struct AskRunIdentityBody {
    pub(super) task_id: String,
    pub(super) answer: String,
    pub(super) answer_kind: AnswerKind,
    pub(super) citations: Vec<CitationResponse>,
    pub(super) verified: bool,
    pub(super) context_pack: Option<ContextPackEnvelope>,
    pub(super) collection_filter: Option<CollectionFilterResponse>,
}

pub(super) fn stamp_ask_run_identity(
    body: &AskRunIdentityBody,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskRun,
        WIRE_SCHEMA_VERSION,
        body.task_id.clone(),
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("ask-run identity does not match the executed response body");
        }
    }
    Ok(expected)
}
