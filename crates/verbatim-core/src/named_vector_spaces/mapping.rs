//! Generation-bound object-to-space routing with no cross-space materialization.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError, NamedVectorSpaceId,
    NamedVectorSpaceResult, ObjectId, PublicationGeneration, VectorRange,
};

/// One compact location in a single homogeneous physical space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VectorLocation {
    Dense {
        shard_ordinal: u32,
        vector_id: u64,
    },
    LateInteraction {
        shard_ordinal: u32,
        range: VectorRange,
    },
}

impl VectorLocation {
    fn validate(&self) -> NamedVectorSpaceResult<()> {
        match self {
            Self::Dense {
                shard_ordinal,
                vector_id,
            } => Self::dense(*shard_ordinal, *vector_id).map(|_| ()),
            Self::LateInteraction {
                shard_ordinal,
                range,
            } => Self::late_interaction(
                *shard_ordinal,
                VectorRange::new(
                    range.start_vector_offset(),
                    range.vector_count(),
                    range.vectors_per_page(),
                )?,
            )
            .map(|_| ()),
        }
    }

    pub fn dense(shard_ordinal: u32, vector_id: u64) -> NamedVectorSpaceResult<Self> {
        if shard_ordinal == 0 || vector_id == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidVectorMapping,
            ));
        }
        Ok(Self::Dense {
            shard_ordinal,
            vector_id,
        })
    }

    pub fn late_interaction(
        shard_ordinal: u32,
        range: VectorRange,
    ) -> NamedVectorSpaceResult<Self> {
        if shard_ordinal == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidVectorMapping,
            ));
        }
        Ok(Self::LateInteraction {
            shard_ordinal,
            range,
        })
    }
}

/// Compact route from one logical object to zero, one, or many representations
/// in exactly one named vector space and generation. It has no pairwise field,
/// thereby preventing cross-space/cross-source materialization by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObjectSpaceMapping {
    object: ObjectId,
    space: NamedVectorSpaceId,
    generation: PublicationGeneration,
    locations: Vec<VectorLocation>,
}

impl<'de> Deserialize<'de> for ObjectSpaceMapping {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            object: ObjectId,
            space: NamedVectorSpaceId,
            generation: PublicationGeneration,
            locations: Vec<VectorLocation>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.object, wire.space, wire.generation, wire.locations)
            .map_err(serde::de::Error::custom)
    }
}

impl ObjectSpaceMapping {
    /// Hard bound keeps route metadata compact even for region-heavy objects.
    pub const MAX_LOCATIONS: usize = 4_096;

    pub fn new(
        object: ObjectId,
        space: NamedVectorSpaceId,
        generation: PublicationGeneration,
        locations: Vec<VectorLocation>,
    ) -> NamedVectorSpaceResult<Self> {
        if locations.len() > Self::MAX_LOCATIONS {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidVectorMapping,
            ));
        }
        let mut seen = BTreeSet::new();
        for location in &locations {
            location.validate()?;
            let key = match location {
                VectorLocation::Dense {
                    shard_ordinal,
                    vector_id,
                } => (0_u8, u64::from(*shard_ordinal), *vector_id),
                VectorLocation::LateInteraction {
                    shard_ordinal,
                    range,
                } => (1_u8, u64::from(*shard_ordinal), range.start_vector_offset()),
            };
            if !seen.insert(key) {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::DuplicateVectorLocation,
                ));
            }
        }
        Ok(Self {
            object,
            space,
            generation,
            locations,
        })
    }

    pub const fn object(&self) -> &ObjectId {
        &self.object
    }
    pub const fn space(&self) -> &NamedVectorSpaceId {
        &self.space
    }
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }
    pub fn locations(&self) -> &[VectorLocation] {
        &self.locations
    }
    pub fn vector_count(&self) -> usize {
        self.locations.len()
    }
}

/// Explicit linear physical-storage accounting across independent spaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageComplexityContract {
    terms: Vec<(u64, u32)>,
}

impl StorageComplexityContract {
    pub const MAX_SPACE_TERMS: usize = 4_096;

    pub fn new(terms: Vec<(u64, u32)>) -> NamedVectorSpaceResult<Self> {
        if terms.is_empty()
            || terms.len() > Self::MAX_SPACE_TERMS
            || terms
                .iter()
                .any(|(count, dimension)| *count == 0 || *dimension == 0)
        {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidStorageComplexity,
            ));
        }
        let contract = Self { terms };
        contract.native_float32_bytes_checked()?;
        Ok(contract)
    }

    /// `O(sum(N_space * D_native))`; no pairwise term exists in this contract.
    pub const fn vector_component_class(&self) -> &'static str {
        "O(sum(N_space * D_native))"
    }

    pub fn native_float32_bytes(&self) -> u64 {
        self.native_float32_bytes_checked().unwrap_or(u64::MAX)
    }

    fn native_float32_bytes_checked(&self) -> NamedVectorSpaceResult<u64> {
        self.terms
            .iter()
            .try_fold(0_u64, |total, (count, dimension)| {
                count
                    .checked_mul(u64::from(*dimension))
                    .and_then(|value| value.checked_mul(4))
                    .and_then(|value| total.checked_add(value))
                    .ok_or_else(|| {
                        NamedVectorSpaceError::contract(
                            NamedVectorSpaceDiagnosticCode::ArithmeticOverflow,
                        )
                    })
            })
    }
}
