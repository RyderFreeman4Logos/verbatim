//! Typed enterprise predicate AST that must be pushed into vector candidate
//! generation before or during traversal — never deferred to post-filter.
//!
//! These are typed Rust enums, not raw backend JSON. The typed `QueryPlan`
//! remains the public contract; backend JSON syntax must not become the stable
//! API.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
};

/// Maximum number of predicates permitted in a bounded conjunction.
pub const MAX_PREDICATES: usize = 16;
/// Maximum byte length of a bounded identifier value.
pub const MAX_IDENTIFIER_BYTES: usize = 256;
/// Maximum byte length of a bounded metadata key.
pub const MAX_METADATA_KEY_BYTES: usize = 128;

/// Lifecycle state permitted in a retrieval predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnterpriseLifecycleState {
    Active,
    Archived,
    Retained,
}

/// Closed typed metadata values permitted in predicates.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum TypedMetadataValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Float(f64),
}

impl TypedMetadataValue {
    fn validate(&self) -> EnterprisePredicateResult<()> {
        match self {
            Self::String(value) if !is_bounded_value(value) => {
                Err(EnterprisePredicateError::contract(
                    EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
                ))
            }
            Self::Float(value) if !value.is_finite() => Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
            )),
            _ => Ok(()),
        }
    }
}

impl fmt::Debug for TypedMetadataValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(_) => formatter.write_str("TypedMetadataValue::String([REDACTED])"),
            Self::Integer(value) => formatter
                .debug_tuple("TypedMetadataValue::Integer")
                .field(value)
                .finish(),
            Self::Boolean(value) => formatter
                .debug_tuple("TypedMetadataValue::Boolean")
                .field(value)
                .finish(),
            Self::Float(_) => formatter.write_str("TypedMetadataValue::Float([REDACTED])"),
        }
    }
}

/// Typed enterprise predicates that must narrow candidate generation before
/// ranking. The enum is the public contract; backend adapters compile it into
/// their own internal representation.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EnterprisePredicate {
    /// Restrict to one source.
    Source { source_id: String },
    /// Restrict to one collection.
    Collection { collection_id: String },
    /// Restrict to one tenant/workspace.
    Tenant { tenant_id: String },
    /// Authorization: principal or group that must be granted read.
    AclPrincipal { principal_or_group: String },
    /// Authorization: deny precedence for principal or group.
    AclDeny { principal_or_group: String },
    /// Restrict to one lifecycle state.
    Lifecycle { lifecycle: EnterpriseLifecycleState },
    /// Effective date/time range in unix milliseconds (inclusive).
    DateRange {
        start_unix_ms: i64,
        end_unix_ms: i64,
    },
    /// Typed metadata equality predicate.
    MetadataEq {
        key: String,
        value: TypedMetadataValue,
    },
}

impl EnterprisePredicate {
    pub fn source(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        Self::named(|source_id| Self::Source { source_id }, value)
    }

    pub fn collection(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        Self::named(|collection_id| Self::Collection { collection_id }, value)
    }

    pub fn tenant(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        Self::named(|tenant_id| Self::Tenant { tenant_id }, value)
    }

    pub fn acl_principal(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        Self::named(
            |principal_or_group| Self::AclPrincipal { principal_or_group },
            value,
        )
    }

    pub fn acl_deny(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        Self::named(
            |principal_or_group| Self::AclDeny { principal_or_group },
            value,
        )
    }

    pub const fn lifecycle(lifecycle: EnterpriseLifecycleState) -> Self {
        Self::Lifecycle { lifecycle }
    }

    pub fn date_range(start_unix_ms: i64, end_unix_ms: i64) -> EnterprisePredicateResult<Self> {
        let predicate = Self::DateRange {
            start_unix_ms,
            end_unix_ms,
        };
        predicate.validate()?;
        Ok(predicate)
    }

    pub fn metadata_eq(
        key: impl Into<String>,
        value: TypedMetadataValue,
    ) -> EnterprisePredicateResult<Self> {
        let predicate = Self::MetadataEq {
            key: key.into(),
            value,
        };
        predicate.validate()?;
        Ok(predicate)
    }

    fn named(
        construct: impl FnOnce(String) -> Self,
        value: impl Into<String>,
    ) -> EnterprisePredicateResult<Self> {
        let predicate = construct(value.into());
        predicate.validate()?;
        Ok(predicate)
    }

    /// Revalidates this predicate before it can constrain candidate generation.
    pub fn validate(&self) -> EnterprisePredicateResult<()> {
        match self {
            Self::Source { source_id }
            | Self::Collection {
                collection_id: source_id,
            }
            | Self::Tenant {
                tenant_id: source_id,
            }
            | Self::AclPrincipal {
                principal_or_group: source_id,
            }
            | Self::AclDeny {
                principal_or_group: source_id,
            } if !is_bounded_value(source_id) => Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
            )),
            Self::DateRange {
                start_unix_ms,
                end_unix_ms,
            } if start_unix_ms > end_unix_ms => Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
            )),
            Self::MetadataEq { key, value } if !is_bounded_metadata_key(key) => {
                Err(EnterprisePredicateError::contract(
                    EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
                ))
            }
            Self::MetadataEq { value, .. } => value.validate(),
            _ => Ok(()),
        }
    }

    /// Returns whether this predicate is a mandatory authorization predicate.
    pub fn is_authorization(&self) -> bool {
        matches!(
            self,
            Self::AclPrincipal { .. } | Self::AclDeny { .. } | Self::Tenant { .. }
        )
    }
}

impl fmt::Debug for EnterprisePredicate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source { .. } => formatter.write_str("EnterprisePredicate::Source([REDACTED])"),
            Self::Collection { .. } => {
                formatter.write_str("EnterprisePredicate::Collection([REDACTED])")
            }
            Self::Tenant { .. } => formatter.write_str("EnterprisePredicate::Tenant([REDACTED])"),
            Self::AclPrincipal { .. } => {
                formatter.write_str("EnterprisePredicate::AclPrincipal([REDACTED])")
            }
            Self::AclDeny { .. } => formatter.write_str("EnterprisePredicate::AclDeny([REDACTED])"),
            Self::Lifecycle { lifecycle } => formatter
                .debug_tuple("EnterprisePredicate::Lifecycle")
                .field(lifecycle)
                .finish(),
            Self::DateRange {
                start_unix_ms,
                end_unix_ms,
            } => formatter
                .debug_struct("EnterprisePredicate::DateRange")
                .field("start_unix_ms", start_unix_ms)
                .field("end_unix_ms", end_unix_ms)
                .finish(),
            Self::MetadataEq { key, value } => formatter
                .debug_struct("EnterprisePredicate::MetadataEq")
                .field("key_len", &key.len())
                .field("value", value)
                .finish(),
        }
    }
}

/// Bounded conjunction of enterprise predicates that must all hold.
#[derive(Clone, PartialEq, Serialize, Default)]
pub struct EnterprisePredicateConjunction {
    predicates: Vec<EnterprisePredicate>,
}

#[derive(Deserialize)]
struct EnterprisePredicateConjunctionFields {
    predicates: Vec<EnterprisePredicate>,
}

impl<'de> Deserialize<'de> for EnterprisePredicateConjunction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = EnterprisePredicateConjunctionFields::deserialize(deserializer)?;
        Self::new(fields.predicates).map_err(serde::de::Error::custom)
    }
}

impl EnterprisePredicateConjunction {
    /// Builds a bounded conjunction and validates every predicate.
    pub fn new(predicates: Vec<EnterprisePredicate>) -> EnterprisePredicateResult<Self> {
        if predicates.len() > MAX_PREDICATES {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::PredicatePayloadTooLarge,
            ));
        }
        for predicate in &predicates {
            predicate.validate()?;
        }
        Ok(Self { predicates })
    }

    /// The predicates that must constrain candidate generation.
    pub fn predicates(&self) -> &[EnterprisePredicate] {
        &self.predicates
    }

    /// Whether any mandatory authorization predicate is present.
    pub fn has_authorization(&self) -> bool {
        self.predicates.iter().any(|p| p.is_authorization())
    }

    /// Number of predicates in the conjunction.
    pub fn len(&self) -> usize {
        self.predicates.len()
    }

    /// Whether the conjunction is empty.
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

impl fmt::Debug for EnterprisePredicateConjunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnterprisePredicateConjunction")
            .field("predicate_count", &self.predicates.len())
            .field("has_authorization", &self.has_authorization())
            .finish()
    }
}

fn is_bounded_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES && value.is_ascii()
}

fn is_bounded_metadata_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_METADATA_KEY_BYTES && key.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_predicate_validates_and_redacts() {
        let predicate = EnterprisePredicate::source("legal").unwrap();
        predicate.validate().unwrap();
        assert!(predicate.is_authorization() == false);
        let debug = format!("{:?}", predicate);
        assert_eq!(debug, "EnterprisePredicate::Source([REDACTED])");
        assert!(!debug.contains("legal"));
    }

    #[test]
    fn empty_source_value_is_rejected() {
        let result = EnterprisePredicate::source("");
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn acl_predicate_is_authorization_and_redacts() {
        let predicate = EnterprisePredicate::acl_principal("group:legal").unwrap();
        assert!(predicate.is_authorization());
        let debug = format!("{:?}", predicate);
        assert_eq!(debug, "EnterprisePredicate::AclPrincipal([REDACTED])");
        assert!(!debug.contains("group:legal"));
    }

    #[test]
    fn date_range_inverted_is_rejected() {
        let result = EnterprisePredicate::date_range(100, 50);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn date_range_debug_keeps_bounds_not_values() {
        let predicate = EnterprisePredicate::date_range(10, 20).unwrap();
        let debug = format!("{:?}", predicate);
        assert!(debug.contains("start_unix_ms: 10"));
        assert!(debug.contains("end_unix_ms: 20"));
    }

    #[test]
    fn metadata_float_redacts_value() {
        let predicate =
            EnterprisePredicate::metadata_eq("score", TypedMetadataValue::Float(1.5)).unwrap();
        let debug = format!("{:?}", predicate);
        assert!(debug.contains("key_len: 5"));
        assert!(debug.contains("Float([REDACTED])"));
        assert!(!debug.contains("1.5"));
    }

    #[test]
    fn metadata_integer_keeps_value() {
        let predicate =
            EnterprisePredicate::metadata_eq("count", TypedMetadataValue::Integer(42)).unwrap();
        let debug = format!("{:?}", predicate);
        assert!(debug.contains("Integer(42)"));
    }

    #[test]
    fn metadata_empty_key_rejected() {
        let result = EnterprisePredicate::metadata_eq("", TypedMetadataValue::Boolean(true));
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn metadata_non_finite_float_rejected() {
        let result = EnterprisePredicate::metadata_eq("x", TypedMetadataValue::Float(f64::NAN));
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn conjunction_redacts_and_reports_count() {
        let conjunction = EnterprisePredicateConjunction::new(vec![
            EnterprisePredicate::source("s1").unwrap(),
            EnterprisePredicate::tenant("t1").unwrap(),
        ])
        .unwrap();
        assert_eq!(conjunction.len(), 2);
        assert!(conjunction.has_authorization());
        let debug = format!("{:?}", conjunction);
        assert!(debug.contains("predicate_count: 2"));
        assert!(debug.contains("has_authorization: true"));
        assert!(!debug.contains("s1"));
        assert!(!debug.contains("t1"));
    }

    #[test]
    fn conjunction_too_many_predicates_rejected() {
        let predicates: Vec<EnterprisePredicate> = (0..=MAX_PREDICATES)
            .map(|i| EnterprisePredicate::source(format!("s{i}")).unwrap())
            .collect();
        let result = EnterprisePredicateConjunction::new(predicates);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::PredicatePayloadTooLarge
        );
    }

    #[test]
    fn lifecycle_predicate_is_not_authorization() {
        let predicate = EnterprisePredicate::lifecycle(EnterpriseLifecycleState::Active);
        assert!(!predicate.is_authorization());
        predicate.validate().unwrap();
    }
}
