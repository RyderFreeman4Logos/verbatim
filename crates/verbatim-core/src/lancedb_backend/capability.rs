//! Capability discovery required before LanceDB is considered a valid reference adapter.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanceDbCapabilityFields {
    pub supports_ivf_rq: bool,
    pub supports_ivf_pq: bool,
    pub supports_hnsw_control: bool,
    pub supports_bypass_exact_scan: bool,
    pub supports_scalar_prefilter: bool,
    pub supports_adaptive_nprobes: bool,
    pub supports_original_vector_rescore: bool,
    pub supports_generation_publication: bool,
    pub supports_optimize_reindex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbCapabilities {
    fields: LanceDbCapabilityFields,
}

impl<'de> Deserialize<'de> for LanceDbCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(LanceDbCapabilityFields::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl LanceDbCapabilities {
    pub fn new(fields: LanceDbCapabilityFields) -> LanceDbBackendResult<Self> {
        if !fields.supports_ivf_rq
            || !fields.supports_ivf_pq
            || !fields.supports_hnsw_control
            || !fields.supports_bypass_exact_scan
            || !fields.supports_scalar_prefilter
            || !fields.supports_adaptive_nprobes
            || !fields.supports_original_vector_rescore
            || !fields.supports_generation_publication
            || !fields.supports_optimize_reindex
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidCapabilities,
            ));
        }
        Ok(Self { fields })
    }

    pub const fn fields(&self) -> &LanceDbCapabilityFields {
        &self.fields
    }
}
