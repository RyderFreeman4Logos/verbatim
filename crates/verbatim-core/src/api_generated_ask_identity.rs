use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{AnswerKind, GeneratedInterpretationResponse};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const LIVE_ASK_GENERATED_INTERPRETATION_ID: &str = "live-ask-generated-interpretation";

/// Serialized generated-answer text with its canonical identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct GeneratedInterpretationWire {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<CanonicalIdentity>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeneratedInterpretationIdentityBody {
    text: String,
}

/// Serializes a generated interpretation with its canonical identity.
pub fn generated_interpretation_wire(
    answer_kind: AnswerKind,
    response: Option<&GeneratedInterpretationResponse>,
) -> Result<Option<GeneratedInterpretationWire>> {
    let Some(response) = response else {
        return Ok(None);
    };
    let body = GeneratedInterpretationIdentityBody {
        text: response.text.clone(),
    };
    let identity = match answer_kind {
        AnswerKind::GeneratedInterpretation => {
            Some(stamp_generated_interpretation_identity(&body, None)?)
        }
        AnswerKind::EvidenceOnly => None,
    };
    Ok(Some(GeneratedInterpretationWire {
        text: body.text,
        identity,
    }))
}

pub(super) fn bind_generated_interpretation_to_answer_kind(
    answer_kind: AnswerKind,
    response: Option<GeneratedInterpretationWire>,
) -> Result<Option<GeneratedInterpretationResponse>> {
    let Some(response) = response else {
        return Ok(None);
    };
    let body = GeneratedInterpretationIdentityBody {
        text: response.text,
    };
    match answer_kind {
        AnswerKind::GeneratedInterpretation => {
            stamp_generated_interpretation_identity(&body, response.identity.as_ref())?;
        }
        AnswerKind::EvidenceOnly if response.identity.is_some() => {
            anyhow::bail!(
                "evidence-only ask response must not carry generated interpretation identity"
            );
        }
        AnswerKind::EvidenceOnly => {}
    }
    Ok(Some(GeneratedInterpretationResponse { text: body.text }))
}

fn stamp_generated_interpretation_identity(
    body: &GeneratedInterpretationIdentityBody,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::DerivedArtifact,
        WIRE_SCHEMA_VERSION,
        LIVE_ASK_GENERATED_INTERPRETATION_ID,
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!(
                "generated interpretation identity does not match the executed answer text"
            );
        }
    }
    Ok(expected)
}
