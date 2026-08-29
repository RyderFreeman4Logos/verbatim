use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::index_gc::{IndexGcApplyReport, IndexGcConfig, IndexGcPlan};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexGcResponse {
    pub dry_run: bool,
    pub policy: IndexGcConfig,
    pub plan: IndexGcPlan,
    pub apply: IndexGcApplyReport,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct IndexGcResponseBody<'a> {
    dry_run: bool,
    policy: &'a IndexGcConfig,
    plan: &'a IndexGcPlan,
    apply: &'a IndexGcApplyReport,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexGcResponseWire {
    dry_run: bool,
    policy: IndexGcConfig,
    plan: IndexGcPlan,
    apply: IndexGcApplyReport,
    identity: CanonicalIdentity,
}

fn index_gc_result_identity(
    dry_run: bool,
    policy: &IndexGcConfig,
    plan: &IndexGcPlan,
    apply: &IndexGcApplyReport,
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::IndexGcResult,
        WIRE_SCHEMA_VERSION,
        "index-gc",
        &encode_wire_document(&IndexGcResponseBody {
            dry_run,
            policy,
            plan,
            apply,
        })?,
    )
}

fn validate_index_gc_result_identity(response: &IndexGcResponse) -> Result<()> {
    response.identity.validate()?;
    let expected = index_gc_result_identity(
        response.dry_run,
        &response.policy,
        &response.plan,
        &response.apply,
    )?;
    if response.identity != expected {
        anyhow::bail!("index-gc-result identity does not match the GC response body");
    }
    Ok(())
}

impl IndexGcResponse {
    pub fn new(
        dry_run: bool,
        policy: IndexGcConfig,
        plan: IndexGcPlan,
        apply: IndexGcApplyReport,
    ) -> Result<Self> {
        let identity = index_gc_result_identity(dry_run, &policy, &plan, &apply)?;
        Ok(Self {
            dry_run,
            policy,
            plan,
            apply,
            identity,
        })
    }
}

impl Serialize for IndexGcResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_index_gc_result_identity(self).map_err(serde::ser::Error::custom)?;
        IndexGcResponseWire {
            dry_run: self.dry_run,
            policy: self.policy.clone(),
            plan: self.plan.clone(),
            apply: self.apply.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IndexGcResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = IndexGcResponseWire::deserialize(deserializer)?;
        let response = Self {
            dry_run: wire.dry_run,
            policy: wire.policy,
            plan: wire.plan,
            apply: wire.apply,
            identity: wire.identity,
        };
        validate_index_gc_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_index_gc_result_identity_wire_tests.rs"]
mod api_index_gc_result_identity_wire_tests;
