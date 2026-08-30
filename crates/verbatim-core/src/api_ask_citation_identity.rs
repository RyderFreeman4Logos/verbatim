use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::CitationResponse;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const ASK_CITATION_EVENT_ARTIFACT_ID: &str = "ask-stream-citation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCitationEvent {
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct AskCitationEventIdentityBody<'a> {
    citations: &'a [CitationResponse],
    verified: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct AskCitationEventWire {
    #[serde(default)]
    citations: Vec<CitationResponse>,
    verified: bool,
    identity: CanonicalIdentity,
}

fn stamp_ask_citation_event_identity(
    citations: &[CitationResponse],
    verified: bool,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let body = AskCitationEventIdentityBody {
        citations,
        verified,
    };
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskCitationEvent,
        WIRE_SCHEMA_VERSION,
        ASK_CITATION_EVENT_ARTIFACT_ID,
        &encode_wire_document(&body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("ask-citation-event identity does not match the citation body");
        }
    }
    Ok(expected)
}

impl AskCitationEvent {
    /// Creates a citation event with its canonical body identity bound.
    pub fn new(citations: Vec<CitationResponse>, verified: bool) -> Result<Self> {
        let identity = stamp_ask_citation_event_identity(&citations, verified, None)?;
        Ok(Self {
            citations,
            verified,
            identity,
        })
    }
}

impl Serialize for AskCitationEvent {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity =
            stamp_ask_citation_event_identity(&self.citations, self.verified, Some(&self.identity))
                .map_err(serde::ser::Error::custom)?;
        AskCitationEventWire {
            citations: self.citations.clone(),
            verified: self.verified,
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskCitationEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskCitationEventWire::deserialize(deserializer)?;
        let identity =
            stamp_ask_citation_event_identity(&wire.citations, wire.verified, Some(&wire.identity))
                .map_err(serde::de::Error::custom)?;
        Ok(Self {
            citations: wire.citations,
            verified: wire.verified,
            identity,
        })
    }
}

#[cfg(test)]
#[path = "api_ask_citation_identity_wire_tests.rs"]
mod wire_tests;
