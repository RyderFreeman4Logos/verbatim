//! Policy and publication generation binding for filter structures and
//! candidate results.
//!
//! Filter structures and query results are bound to exactly one policy
//! generation and one publication generation. A mismatch is a closed failure;
//! it cannot combine old and new filter structures or shards.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
};

/// Non-zero, monotonically useful authorization policy generation.
///
/// Filter structures carry this binding so that a stale ACL page or a query
/// issued against an older policy cannot mix with a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PolicyGeneration(u64);

impl<'de> Deserialize<'de> for PolicyGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PolicyGeneration {
    /// Creates a non-zero policy generation.
    pub fn new(value: u64) -> EnterprisePredicateResult<Self> {
        if value == 0 {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::GenerationBindingInvalid,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the policy generation number.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Non-zero publication generation (index/shard publication).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PublicationGenerationBinding(u64);

impl<'de> Deserialize<'de> for PublicationGenerationBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PublicationGenerationBinding {
    /// Creates a non-zero publication generation binding.
    pub fn new(value: u64) -> EnterprisePredicateResult<Self> {
        if value == 0 {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::GenerationBindingInvalid,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the publication generation number.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Immutable binding pairing one policy generation with one publication
/// generation. Filter structures and candidate results both carry this binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct GenerationBinding {
    policy: PolicyGeneration,
    publication: PublicationGenerationBinding,
}

impl<'de> Deserialize<'de> for GenerationBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            policy: PolicyGeneration,
            publication: PublicationGenerationBinding,
        }
        let fields = Fields::deserialize(deserializer)?;
        Self::new(fields.policy, fields.publication).map_err(serde::de::Error::custom)
    }
}

impl GenerationBinding {
    /// Creates a validated generation binding.
    pub fn new(
        policy: PolicyGeneration,
        publication: PublicationGenerationBinding,
    ) -> EnterprisePredicateResult<Self> {
        // Both are already non-zero by construction; this is a defensive
        // double-check at the composite boundary.
        if policy.value() == 0 || publication.value() == 0 {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::GenerationBindingInvalid,
            ));
        }
        Ok(Self {
            policy,
            publication,
        })
    }

    /// Returns the bound authorization policy generation.
    pub const fn policy(&self) -> PolicyGeneration {
        self.policy
    }

    /// Returns the bound index publication generation.
    pub const fn publication(&self) -> PublicationGenerationBinding {
        self.publication
    }

    /// Whether two bindings are compatible (exactly equal). Mixed generations
    /// are a closed failure.
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self == other
    }
}

impl fmt::Debug for GenerationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Policy and publication generation numbers are monotonically
        // increasing internal counters, not caller-controlled secrets.
        formatter
            .debug_struct("GenerationBinding")
            .field("policy", &self.policy)
            .field("publication", &self.publication)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_generation_rejects_zero() {
        let result = PolicyGeneration::new(0);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::GenerationBindingInvalid
        );
    }

    #[test]
    fn publication_generation_rejects_zero() {
        let result = PublicationGenerationBinding::new(0);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::GenerationBindingInvalid
        );
    }

    #[test]
    fn binding_is_compatible_only_with_equal() {
        let a = GenerationBinding::new(
            PolicyGeneration::new(1).unwrap(),
            PublicationGenerationBinding::new(2).unwrap(),
        )
        .unwrap();
        let b = GenerationBinding::new(
            PolicyGeneration::new(1).unwrap(),
            PublicationGenerationBinding::new(2).unwrap(),
        )
        .unwrap();
        assert!(a.is_compatible_with(&b));

        let c = GenerationBinding::new(
            PolicyGeneration::new(3).unwrap(),
            PublicationGenerationBinding::new(2).unwrap(),
        )
        .unwrap();
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn mixed_publication_generations_incompatible() {
        let a = GenerationBinding::new(
            PolicyGeneration::new(1).unwrap(),
            PublicationGenerationBinding::new(2).unwrap(),
        )
        .unwrap();
        let b = GenerationBinding::new(
            PolicyGeneration::new(1).unwrap(),
            PublicationGenerationBinding::new(3).unwrap(),
        )
        .unwrap();
        assert!(!a.is_compatible_with(&b));
    }

    #[test]
    fn binding_debug_includes_generations() {
        let binding = GenerationBinding::new(
            PolicyGeneration::new(7).unwrap(),
            PublicationGenerationBinding::new(9).unwrap(),
        )
        .unwrap();
        let debug = format!("{:?}", binding);
        assert!(debug.contains("policy: PolicyGeneration(7)"));
        assert!(debug.contains("publication: PublicationGenerationBinding(9)"));
    }
}
