use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::types::RetrievalDebug;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WireEnvelopeHeader,
    WireEnvelopeHeaderFields, WIRE_SCHEMA_VERSION,
};

const ASK_RETRIEVAL_DEBUG_EVENT_ARTIFACT_ID: &str = "ask-stream-retrieval-debug";

/// Retrieval diagnostics published by the ask stream with a bound wire identity.
#[derive(Debug, Clone, PartialEq)]
pub struct AskRetrievalDebugEvent {
    pub debug: RetrievalDebug,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct AskRetrievalDebugEventWireRef<'a> {
    #[serde(flatten)]
    debug: &'a RetrievalDebug,
    identity: &'a CanonicalIdentity,
}

#[derive(Debug, Deserialize)]
struct AskRetrievalDebugEventWire {
    #[serde(flatten)]
    debug: RetrievalDebug,
    identity: CanonicalIdentity,
}

fn validate_identity_header(identity: &CanonicalIdentity) -> Result<()> {
    WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
        identity: identity.clone(),
        generation: None,
        profile_ref: None,
    })?
    .validate()
}

fn stamp_ask_retrieval_debug_event_identity(
    debug: &RetrievalDebug,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskRetrievalDebugEvent,
        WIRE_SCHEMA_VERSION,
        ASK_RETRIEVAL_DEBUG_EVENT_ARTIFACT_ID,
        &encode_wire_document(debug)?,
    )?;
    validate_identity_header(&expected)?;
    if let Some(supplied) = supplied {
        validate_identity_header(supplied)?;
        if supplied != &expected {
            anyhow::bail!("ask-retrieval-debug-event identity does not match the debug body");
        }
    }
    Ok(expected)
}

impl AskRetrievalDebugEvent {
    /// Creates an event with identity bound to all retrieval diagnostics.
    pub fn new(debug: RetrievalDebug) -> Result<Self> {
        let identity = stamp_ask_retrieval_debug_event_identity(&debug, None)?;
        Ok(Self { debug, identity })
    }

    /// Returns the unchanged retrieval diagnostics carried by this event.
    pub fn into_retrieval_debug(self) -> RetrievalDebug {
        self.debug
    }
}

impl Serialize for AskRetrievalDebugEvent {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = stamp_ask_retrieval_debug_event_identity(&self.debug, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        AskRetrievalDebugEventWireRef {
            debug: &self.debug,
            identity: &identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskRetrievalDebugEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskRetrievalDebugEventWire::deserialize(deserializer)?;
        let identity = stamp_ask_retrieval_debug_event_identity(&wire.debug, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            debug: wire.debug,
            identity,
        })
    }
}

#[cfg(test)]
#[path = "api_ask_retrieval_debug_identity_wire_tests.rs"]
mod wire_tests;
