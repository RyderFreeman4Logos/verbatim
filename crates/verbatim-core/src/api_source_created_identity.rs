use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddSourceResponse {
    pub id: String,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Clone, Serialize)]
struct AddSourceResponseBody<'a> {
    id: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddSourceResponseWire {
    id: String,
    identity: CanonicalIdentity,
}

fn stamp_source_created_identity(
    id: &str,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::SourceCreated,
        WIRE_SCHEMA_VERSION,
        id,
        &encode_wire_document(&AddSourceResponseBody { id })?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!(
                "source-created identity does not match the source-created response body"
            );
        }
    }
    Ok(expected)
}

impl AddSourceResponse {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        let identity = stamp_source_created_identity(&id, None)?;
        Ok(Self { id, identity })
    }
}

impl Serialize for AddSourceResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = stamp_source_created_identity(&self.id, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        AddSourceResponseWire {
            id: self.id.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AddSourceResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AddSourceResponseWire::deserialize(deserializer)?;
        let identity = stamp_source_created_identity(&wire.id, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            id: wire.id,
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    include!("api_source_created_identity_wire_tests.rs");
}
