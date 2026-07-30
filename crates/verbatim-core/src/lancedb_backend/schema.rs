//! Fixed full-dimensional LanceDB table schema.

use serde::{Deserialize, Serialize};

use super::{
    LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult,
    LanceDbCollectionIdentity,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanceDbSchemaFields {
    pub dimension: u32,
    pub original_vectors_f32_retained: bool,
    pub full_dimension_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbCollectionSchema {
    identity: LanceDbCollectionIdentity,
    fields: LanceDbSchemaFields,
}

impl<'de> Deserialize<'de> for LanceDbCollectionSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            identity: LanceDbCollectionIdentity,
            fields: LanceDbSchemaFields,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.identity, wire.fields).map_err(serde::de::Error::custom)
    }
}

impl LanceDbCollectionSchema {
    pub const DIMENSION: u32 = 4_096;

    pub fn new(
        identity: LanceDbCollectionIdentity,
        fields: LanceDbSchemaFields,
    ) -> LanceDbBackendResult<Self> {
        if fields.dimension != Self::DIMENSION {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::VectorDimensionMismatch,
            ));
        }
        if !fields.full_dimension_required || !fields.original_vectors_f32_retained {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidSchema,
            ));
        }
        Ok(Self { identity, fields })
    }

    pub const fn identity(&self) -> &LanceDbCollectionIdentity {
        &self.identity
    }

    pub const fn fields(&self) -> &LanceDbSchemaFields {
        &self.fields
    }

    pub const fn dimension(&self) -> u32 {
        self.fields.dimension
    }
}
