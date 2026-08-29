use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::{
    validate_collection_name, CollectionMember, CollectionRecord, CollectionRoot,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct CollectionResponseBody<'a> {
    collection: &'a CollectionRecord,
    roots: &'a [CollectionRoot],
    members: &'a [CollectionMember],
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionResponseWire {
    collection: CollectionRecord,
    #[serde(default)]
    roots: Vec<CollectionRoot>,
    #[serde(default)]
    members: Vec<CollectionMember>,
    identity: CanonicalIdentity,
}

fn collection_result_identity(
    collection: &CollectionRecord,
    roots: &[CollectionRoot],
    members: &[CollectionMember],
) -> Result<CanonicalIdentity> {
    validate_collection_name(&collection.name)?;
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionResult,
        WIRE_SCHEMA_VERSION,
        &collection.name,
        &encode_wire_document(&CollectionResponseBody {
            collection,
            roots,
            members,
        })?,
    )
}

fn validate_collection_result_identity(response: &super::CollectionResponse) -> Result<()> {
    validate_collection_name(&response.collection.name)?;
    if response.identity.artifact_id != response.collection.name {
        anyhow::bail!("collection-result identity does not match the collection name");
    }
    response.identity.validate()?;
    let expected =
        collection_result_identity(&response.collection, &response.roots, &response.members)?;
    if response.identity != expected {
        anyhow::bail!("collection-result identity does not match the collection response body");
    }
    Ok(())
}

impl super::CollectionResponse {
    pub fn new(
        collection: CollectionRecord,
        roots: Vec<CollectionRoot>,
        members: Vec<CollectionMember>,
    ) -> Result<Self> {
        let identity = collection_result_identity(&collection, &roots, &members)?;
        Ok(Self {
            collection,
            roots,
            members,
            identity,
        })
    }

    pub fn validate_for_collection(&self, collection_name: &str) -> Result<()> {
        validate_collection_name(collection_name)?;
        if self.identity.artifact_id != collection_name {
            anyhow::bail!("collection-result identity does not match the requested collection");
        }
        validate_collection_result_identity(self)
    }
}

impl Serialize for super::CollectionResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_result_identity(self).map_err(serde::ser::Error::custom)?;
        CollectionResponseWire {
            collection: self.collection.clone(),
            roots: self.roots.clone(),
            members: self.members.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::CollectionResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionResponseWire::deserialize(deserializer)?;
        let response = Self {
            collection: wire.collection,
            roots: wire.roots,
            members: wire.members,
            identity: wire.identity,
        };
        validate_collection_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_collection_result_identity_wire_tests.rs"]
mod api_collection_result_identity_wire_tests;
