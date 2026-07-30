//! Native-dimensional, backend-neutral named-vector-space specifications.

use serde::{Deserialize, Serialize};

use super::{
    CandidateIndexProfileId, ModelIdentity, NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError,
    NamedVectorSpaceId, NamedVectorSpaceResult, PublicationGeneration,
};

/// Modality and role of one homogeneous physical vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingModality {
    Text,
    Title,
    Image,
    Layout,
    Audio,
    Domain,
    AsymmetricQuery,
    AsymmetricDocument,
    LateInteractionToken,
    Classification,
    DuplicateDetection,
}

/// Metric used only within one compatible physical space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorMetric {
    Cosine,
    DotProduct,
    Euclidean,
}

/// Normalization promised by the encoder and stored vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    None,
    L2,
}

/// SSD encoding for candidate storage. Float32 retains original native vectors
/// for exact scoring; no variant represents dimensional truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageEncoding {
    Float32,
    Float16,
    ProductQuantized,
    BinaryCandidateCode,
}

/// Operation a named space may accept from a typed query plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOperation {
    DenseNearestNeighbor,
    FilteredDenseNearestNeighbor,
    LateInteractionMaxSim,
    Classification,
    DuplicateDetection,
}

/// Serializable field bag for [`NamedVectorSpaceSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedVectorSpaceSpecFields {
    pub name: NamedVectorSpaceId,
    pub modality: EmbeddingModality,
    pub model: ModelIdentity,
    pub native_dimension: u32,
    pub metric: VectorMetric,
    pub normalization: Normalization,
    pub storage_encoding: StorageEncoding,
    pub candidate_index_profile: CandidateIndexProfileId,
    pub generation: PublicationGeneration,
    pub supported_operations: Vec<QueryOperation>,
}

/// Complete versioned specification for one homogeneous named vector space.
///
/// `native_dimension` is the encoder's complete native dimension. This contract
/// intentionally has no reduction/MRL field or adapter hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedVectorSpaceSpec {
    name: NamedVectorSpaceId,
    modality: EmbeddingModality,
    model: ModelIdentity,
    native_dimension: u32,
    metric: VectorMetric,
    normalization: Normalization,
    storage_encoding: StorageEncoding,
    candidate_index_profile: CandidateIndexProfileId,
    generation: PublicationGeneration,
    supported_operations: Vec<QueryOperation>,
}

impl<'de> Deserialize<'de> for NamedVectorSpaceSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(NamedVectorSpaceSpecFields::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl NamedVectorSpaceSpec {
    /// Builds a validated, generation-bound homogeneous space specification.
    pub fn new(fields: NamedVectorSpaceSpecFields) -> NamedVectorSpaceResult<Self> {
        if fields.native_dimension == 0 || fields.native_dimension > 1_000_000 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidNativeDimension,
            ));
        }
        if fields.supported_operations.is_empty()
            || has_duplicates(&fields.supported_operations)
            || (fields.modality == EmbeddingModality::LateInteractionToken
                && !fields
                    .supported_operations
                    .contains(&QueryOperation::LateInteractionMaxSim))
            || (fields.modality != EmbeddingModality::LateInteractionToken
                && fields
                    .supported_operations
                    .contains(&QueryOperation::LateInteractionMaxSim))
        {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidSpaceSpecification,
            ));
        }
        Ok(Self {
            name: fields.name,
            modality: fields.modality,
            model: fields.model,
            native_dimension: fields.native_dimension,
            metric: fields.metric,
            normalization: fields.normalization,
            storage_encoding: fields.storage_encoding,
            candidate_index_profile: fields.candidate_index_profile,
            generation: fields.generation,
            supported_operations: fields.supported_operations,
        })
    }

    pub fn into_fields(self) -> NamedVectorSpaceSpecFields {
        NamedVectorSpaceSpecFields {
            name: self.name,
            modality: self.modality,
            model: self.model,
            native_dimension: self.native_dimension,
            metric: self.metric,
            normalization: self.normalization,
            storage_encoding: self.storage_encoding,
            candidate_index_profile: self.candidate_index_profile,
            generation: self.generation,
            supported_operations: self.supported_operations,
        }
    }

    pub const fn name(&self) -> &NamedVectorSpaceId {
        &self.name
    }
    pub const fn modality(&self) -> EmbeddingModality {
        self.modality
    }
    pub const fn model(&self) -> &ModelIdentity {
        &self.model
    }
    pub const fn native_dimension(&self) -> u32 {
        self.native_dimension
    }
    pub const fn metric(&self) -> VectorMetric {
        self.metric
    }
    pub const fn normalization(&self) -> Normalization {
        self.normalization
    }
    pub const fn storage_encoding(&self) -> StorageEncoding {
        self.storage_encoding
    }
    pub const fn candidate_index_profile(&self) -> &CandidateIndexProfileId {
        &self.candidate_index_profile
    }
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }
    pub fn supported_operations(&self) -> &[QueryOperation] {
        &self.supported_operations
    }

    pub fn supports(&self, operation: QueryOperation) -> bool {
        self.supported_operations.contains(&operation)
    }
}

fn has_duplicates(values: &[QueryOperation]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}
