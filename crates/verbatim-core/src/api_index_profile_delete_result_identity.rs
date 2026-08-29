use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::index_profile_delete::{IndexProfileDeleteApplyReport, IndexProfileDeletePlan};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexProfileDeleteResponse {
    pub dry_run: bool,
    pub plan: IndexProfileDeletePlan,
    pub apply: IndexProfileDeleteApplyReport,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct IndexProfileDeleteResponseBody<'a> {
    dry_run: bool,
    plan: &'a IndexProfileDeletePlan,
    apply: &'a IndexProfileDeleteApplyReport,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexProfileDeleteResponseWire {
    dry_run: bool,
    plan: IndexProfileDeletePlan,
    apply: IndexProfileDeleteApplyReport,
    identity: CanonicalIdentity,
}

fn index_profile_delete_result_identity(
    dry_run: bool,
    plan: &IndexProfileDeletePlan,
    apply: &IndexProfileDeleteApplyReport,
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::IndexProfileDeleteResult,
        WIRE_SCHEMA_VERSION,
        "index-profile-delete",
        &encode_wire_document(&IndexProfileDeleteResponseBody {
            dry_run,
            plan,
            apply,
        })?,
    )
}

fn validate_index_profile_delete_result_identity(
    response: &IndexProfileDeleteResponse,
) -> Result<()> {
    response.identity.validate()?;
    let expected =
        index_profile_delete_result_identity(response.dry_run, &response.plan, &response.apply)?;
    if response.identity != expected {
        anyhow::bail!(
            "index-profile-delete-result identity does not match the deletion response body"
        );
    }
    Ok(())
}

impl IndexProfileDeleteResponse {
    pub fn new(
        dry_run: bool,
        plan: IndexProfileDeletePlan,
        apply: IndexProfileDeleteApplyReport,
    ) -> Result<Self> {
        let identity = index_profile_delete_result_identity(dry_run, &plan, &apply)?;
        Ok(Self {
            dry_run,
            plan,
            apply,
            identity,
        })
    }
}

impl Serialize for IndexProfileDeleteResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_index_profile_delete_result_identity(self).map_err(serde::ser::Error::custom)?;
        IndexProfileDeleteResponseWire {
            dry_run: self.dry_run,
            plan: self.plan.clone(),
            apply: self.apply.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IndexProfileDeleteResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = IndexProfileDeleteResponseWire::deserialize(deserializer)?;
        let response = Self {
            dry_run: wire.dry_run,
            plan: wire.plan,
            apply: wire.apply,
            identity: wire.identity,
        };
        validate_index_profile_delete_result_identity(&response)
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_index_profile_delete_result_identity_wire_tests.rs"]
mod api_index_profile_delete_result_identity_wire_tests;
