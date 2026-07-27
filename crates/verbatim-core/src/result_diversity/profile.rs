//! Versioned, hash-bound diversity policy configuration.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DiversityError, DiversityResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityProfileFields {
    pub version: u32,
    pub near_duplicate_threshold_basis_points: u16,
    pub max_per_source: Option<u16>,
    pub max_per_thread: Option<u16>,
    pub enable_mmr: bool,
}

/// Versioned presentation/context policy. The hash is computed from exactly the
/// public fields above; it is an audit binding, not a secret or security key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityProfile {
    version: u32,
    profile_hash: String,
    near_duplicate_threshold_basis_points: u16,
    max_per_source: Option<u16>,
    max_per_thread: Option<u16>,
    enable_mmr: bool,
}

impl DiversityProfile {
    pub fn new(fields: DiversityProfileFields) -> DiversityResult<Self> {
        validate_fields(&fields)?;
        let profile_hash = Self::hash_fields(&fields)?;
        Ok(Self {
            version: fields.version,
            profile_hash,
            near_duplicate_threshold_basis_points: fields.near_duplicate_threshold_basis_points,
            max_per_source: fields.max_per_source,
            max_per_thread: fields.max_per_thread,
            enable_mmr: fields.enable_mmr,
        })
    }

    pub fn hash_fields(fields: &DiversityProfileFields) -> DiversityResult<String> {
        validate_fields(fields)?;
        let canonical = serde_json::to_vec(fields).map_err(|_| {
            DiversityError::validation("result-diversity profile could not be serialized")
        })?;
        Ok(format!("{:x}", Sha256::digest(canonical)))
    }

    pub fn validate(&self) -> DiversityResult<()> {
        let fields = DiversityProfileFields {
            version: self.version,
            near_duplicate_threshold_basis_points: self.near_duplicate_threshold_basis_points,
            max_per_source: self.max_per_source,
            max_per_thread: self.max_per_thread,
            enable_mmr: self.enable_mmr,
        };
        let expected_hash = Self::hash_fields(&fields)?;
        if self.profile_hash != expected_hash {
            return Err(DiversityError::validation(
                "result-diversity profile hash does not match profile fields",
            ));
        }
        Ok(())
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn profile_hash(&self) -> &str {
        &self.profile_hash
    }

    pub fn near_duplicate_threshold_basis_points(&self) -> u16 {
        self.near_duplicate_threshold_basis_points
    }

    pub fn max_per_source(&self) -> Option<u16> {
        self.max_per_source
    }

    pub fn max_per_thread(&self) -> Option<u16> {
        self.max_per_thread
    }

    pub fn enable_mmr(&self) -> bool {
        self.enable_mmr
    }
}

fn validate_fields(fields: &DiversityProfileFields) -> DiversityResult<()> {
    if fields.version == 0 {
        return Err(DiversityError::validation(
            "result-diversity profile version must be positive",
        ));
    }
    if fields.near_duplicate_threshold_basis_points == 0
        || fields.near_duplicate_threshold_basis_points > 10_000
    {
        return Err(DiversityError::validation(
            "near-duplicate threshold must be between 1 and 10000 basis points",
        ));
    }
    if fields.max_per_source == Some(0) || fields.max_per_thread == Some(0) {
        return Err(DiversityError::validation(
            "result-diversity quotas must be positive when configured",
        ));
    }
    Ok(())
}
