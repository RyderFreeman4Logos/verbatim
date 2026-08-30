use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::CollectionRecord;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionListResponse {
    pub collections: Vec<CollectionRecord>,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct CollectionListResponseBody<'a> {
    collections: &'a [CollectionRecord],
}

fn collection_list_result_identity(collections: &[CollectionRecord]) -> Result<CanonicalIdentity> {
    let body = CollectionListResponseBody { collections };
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionListResult,
        WIRE_SCHEMA_VERSION,
        "collections",
        &encode_wire_document(&body)?,
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionListResponseWire {
    collections: Vec<CollectionRecord>,
    identity: CanonicalIdentity,
}

impl CollectionListResponse {
    pub fn new(collections: Vec<CollectionRecord>) -> Result<Self> {
        let identity = collection_list_result_identity(&collections)?;
        Ok(Self {
            collections,
            identity,
        })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        let expected = collection_list_result_identity(&self.collections)?;
        self.identity.validate()?;
        if self.identity != expected {
            anyhow::bail!(
                "collection-list identity does not match the collection list response body"
            );
        }
        Ok(expected)
    }
}

impl Serialize for CollectionListResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        CollectionListResponseWire {
            collections: self.collections.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CollectionListResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionListResponseWire::deserialize(deserializer)?;
        let response = Self {
            collections: wire.collections,
            identity: wire.identity,
        };
        response
            .stamp_identity()
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_collection_list_result_identity_wire_tests.rs"]
mod wire_tests;
