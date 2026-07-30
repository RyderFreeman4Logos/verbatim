//! Validated opaque identifiers and generation binding.

use serde::{Deserialize, Serialize};

use super::{NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError, NamedVectorSpaceResult};

fn validate_identifier(value: &str, maximum: usize) -> NamedVectorSpaceResult<()> {
    let valid = !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(NamedVectorSpaceError::contract(
            NamedVectorSpaceDiagnosticCode::InvalidIdentifier,
        ))
    }
}

macro_rules! opaque_identifier {
    ($name:ident, $maximum:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                String::deserialize(deserializer)
                    .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
            }
        }

        impl $name {
            pub fn new(value: impl Into<String>) -> NamedVectorSpaceResult<Self> {
                let value = value.into();
                validate_identifier(&value, $maximum)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

// Backend-neutral named vector-space identity.
opaque_identifier!(NamedVectorSpaceId, 128);
// Model or document-encoder profile identity; it contains no secret endpoint.
opaque_identifier!(ModelIdentity, 192);
// Versioned candidate-index profile identity.
opaque_identifier!(CandidateIndexProfileId, 128);
// Stable logical chunk/evidence object identity.
opaque_identifier!(ObjectId, 192);

/// Monotonic nonzero publication generation. A reader binds every space and
/// mapping to one value and never mixes values inside a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PublicationGeneration(u64);

impl<'de> Deserialize<'de> for PublicationGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        u64::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
    }
}

impl PublicationGeneration {
    pub fn new(value: u64) -> NamedVectorSpaceResult<Self> {
        if value == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidGeneration,
            ));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}
