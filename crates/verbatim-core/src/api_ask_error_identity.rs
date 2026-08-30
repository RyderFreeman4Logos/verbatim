use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const ASK_ERROR_EVENT_ARTIFACT_ID: &str = "ask-stream-error";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskErrorEvent {
    pub status: Option<u16>,
    pub error: String,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct AskErrorEventIdentityBody<'a> {
    status: Option<u16>,
    error: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
struct AskErrorEventWire {
    status: Option<u16>,
    error: String,
    identity: CanonicalIdentity,
}

fn stamp_ask_error_event_identity(
    body: &AskErrorEventIdentityBody<'_>,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskErrorEvent,
        WIRE_SCHEMA_VERSION,
        ASK_ERROR_EVENT_ARTIFACT_ID,
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("ask-error-event identity does not match the error body");
        }
    }
    Ok(expected)
}

impl AskErrorEvent {
    /// Creates an error event with its canonical body identity bound.
    pub fn new(status: Option<u16>, error: impl Into<String>) -> Result<Self> {
        let error = error.into();
        let body = AskErrorEventIdentityBody {
            status,
            error: &error,
        };
        let identity = stamp_ask_error_event_identity(&body, None)?;
        Ok(Self {
            status,
            error,
            identity,
        })
    }
}

impl Serialize for AskErrorEvent {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let body = AskErrorEventIdentityBody {
            status: self.status,
            error: &self.error,
        };
        let identity = stamp_ask_error_event_identity(&body, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        AskErrorEventWire {
            status: self.status,
            error: self.error.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskErrorEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskErrorEventWire::deserialize(deserializer)?;
        let body = AskErrorEventIdentityBody {
            status: wire.status,
            error: &wire.error,
        };
        let identity = stamp_ask_error_event_identity(&body, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            status: wire.status,
            error: wire.error,
            identity,
        })
    }
}

#[cfg(test)]
#[path = "api_ask_error_identity_wire_tests.rs"]
mod wire_tests;
