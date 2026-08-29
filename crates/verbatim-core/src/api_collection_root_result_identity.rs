use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::{validate_collection_name, CollectionRoot};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddCollectionRootResponse {
    pub collection_name: String,
    pub root: CollectionRoot,
    pub root_count: usize,
    pub member_count: usize,
    pub added: bool,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Clone, Serialize)]
struct AddCollectionRootResponseBody<'a> {
    collection_name: &'a str,
    root: &'a CollectionRoot,
    root_count: usize,
    member_count: usize,
    added: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddCollectionRootResponseWire {
    collection_name: String,
    root: CollectionRoot,
    root_count: usize,
    member_count: usize,
    added: bool,
    identity: CanonicalIdentity,
}

fn collection_root_result_identity(
    collection_name: &str,
    root: &CollectionRoot,
    root_count: usize,
    member_count: usize,
    added: bool,
) -> Result<CanonicalIdentity> {
    validate_collection_name(collection_name)?;
    if root.collection_name != collection_name {
        anyhow::bail!("collection-root-result name does not match the root collection name");
    }
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionRootResult,
        WIRE_SCHEMA_VERSION,
        collection_name,
        &encode_wire_document(&AddCollectionRootResponseBody {
            collection_name,
            root,
            root_count,
            member_count,
            added,
        })?,
    )
}

fn validate_collection_root_result_identity(response: &AddCollectionRootResponse) -> Result<()> {
    validate_collection_name(&response.identity.artifact_id)?;
    if response.identity.artifact_id != response.collection_name {
        anyhow::bail!(
            "collection-root-result identity does not match the response collection name"
        );
    }
    response.identity.validate()?;
    let expected = collection_root_result_identity(
        &response.collection_name,
        &response.root,
        response.root_count,
        response.member_count,
        response.added,
    )?;
    if response.identity != expected {
        anyhow::bail!("collection-root-result identity does not match the add-root response body");
    }
    Ok(())
}

#[cfg(test)]
#[path = "api_collection_root_result_identity_wire_tests.rs"]
mod api_collection_root_result_identity_wire_tests;

impl AddCollectionRootResponse {
    pub fn new(
        collection_name: impl Into<String>,
        root: CollectionRoot,
        root_count: usize,
        member_count: usize,
        added: bool,
    ) -> Result<Self> {
        let collection_name = collection_name.into();
        let identity = collection_root_result_identity(
            &collection_name,
            &root,
            root_count,
            member_count,
            added,
        )?;
        Ok(Self {
            collection_name,
            root,
            root_count,
            member_count,
            added,
            identity,
        })
    }

    pub fn validate_for_collection(&self, collection_name: &str) -> Result<()> {
        validate_collection_name(collection_name)?;
        if self.identity.artifact_id != collection_name {
            anyhow::bail!(
                "collection-root-result identity does not match the requested collection"
            );
        }
        validate_collection_root_result_identity(self)
    }
}

impl Serialize for AddCollectionRootResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_root_result_identity(self).map_err(serde::ser::Error::custom)?;
        AddCollectionRootResponseWire {
            collection_name: self.collection_name.clone(),
            root: self.root.clone(),
            root_count: self.root_count,
            member_count: self.member_count,
            added: self.added,
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AddCollectionRootResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AddCollectionRootResponseWire::deserialize(deserializer)?;
        let response = Self {
            collection_name: wire.collection_name,
            root: wire.root,
            root_count: wire.root_count,
            member_count: wire.member_count,
            added: wire.added,
            identity: wire.identity,
        };
        validate_collection_root_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
