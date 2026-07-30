//! Validated, bounded identities for one service generation.

use serde::{Deserialize, Serialize};

use super::{DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult};

const MAX_ID_LEN: usize = 128;

macro_rules! bounded_identity {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            pub fn new(value: impl Into<String>) -> DiskAnn3ServiceResult<Self> {
                let value = value.into();
                if !is_bounded_id(&value) {
                    return Err(DiskAnn3ServiceError::contract(
                        DiskAnn3ServiceDiagnosticCode::InvalidIdentity,
                    ));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

bounded_identity!(ServiceIdentity);
bounded_identity!(VectorSpaceId);
bounded_identity!(ProfileId);
bounded_identity!(IdempotencyKey);

/// Immutable, nonzero publication generation. Readers bind to exactly one generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Generation(u64);

impl<'de> Deserialize<'de> for Generation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Generation {
    pub fn new(value: u64) -> DiskAnn3ServiceResult<Self> {
        if value == 0 {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Identity that all request, shard, replica, and response boundaries must preserve.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RequestIdentity {
    service: ServiceIdentity,
    vector_space: VectorSpaceId,
    profile: ProfileId,
    generation: Generation,
}

impl<'de> Deserialize<'de> for RequestIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            service: ServiceIdentity,
            vector_space: VectorSpaceId,
            profile: ProfileId,
            generation: Generation,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.service,
            wire.vector_space,
            wire.profile,
            wire.generation,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl RequestIdentity {
    pub fn new(
        service: ServiceIdentity,
        vector_space: VectorSpaceId,
        profile: ProfileId,
        generation: Generation,
    ) -> DiskAnn3ServiceResult<Self> {
        Ok(Self {
            service,
            vector_space,
            profile,
            generation,
        })
    }

    pub const fn service(&self) -> &ServiceIdentity {
        &self.service
    }
    pub const fn vector_space(&self) -> &VectorSpaceId {
        &self.vector_space
    }
    pub const fn profile(&self) -> &ProfileId {
        &self.profile
    }
    pub const fn generation(&self) -> Generation {
        self.generation
    }
}

fn is_bounded_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_LEN
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
