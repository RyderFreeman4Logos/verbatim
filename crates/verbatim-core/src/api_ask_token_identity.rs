use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const ASK_TOKEN_EVENT_ARTIFACT_ID: &str = "ask-stream-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskTokenEvent {
    pub text: String,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct AskTokenEventIdentityBody<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
struct AskTokenEventWire {
    text: String,
    identity: CanonicalIdentity,
}

fn stamp_ask_token_event_identity(
    body: &AskTokenEventIdentityBody<'_>,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskTokenEvent,
        WIRE_SCHEMA_VERSION,
        ASK_TOKEN_EVENT_ARTIFACT_ID,
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("ask-token-event identity does not match the token body");
        }
    }
    Ok(expected)
}

impl AskTokenEvent {
    /// Creates a token event with its canonical body identity bound.
    pub fn new(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        let body = AskTokenEventIdentityBody { text: &text };
        let identity = stamp_ask_token_event_identity(&body, None)?;
        Ok(Self { text, identity })
    }
}

impl Serialize for AskTokenEvent {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let body = AskTokenEventIdentityBody { text: &self.text };
        let identity = stamp_ask_token_event_identity(&body, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        AskTokenEventWire {
            text: self.text.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskTokenEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskTokenEventWire::deserialize(deserializer)?;
        let body = AskTokenEventIdentityBody { text: &wire.text };
        let identity = stamp_ask_token_event_identity(&body, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            text: wire.text,
            identity,
        })
    }
}

#[cfg(test)]
#[path = "api_ask_token_identity_wire_tests.rs"]
mod wire_tests;
