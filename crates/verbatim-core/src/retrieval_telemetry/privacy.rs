//! Privacy policy and opaque correlation IDs for retrieval telemetry.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TelemetryDiagnosticCode, TelemetryError, TelemetryResult};

/// Maximum source bytes accepted before deriving a redacted correlation token.
pub const MAX_REDACTED_TELEMETRY_ID_SOURCE_BYTES: usize = 256;

const REDACTED_TOKEN_PREFIX: &str = "rtid_";
const REDACTED_TOKEN_HEX_BYTES: usize = 16;
const REDACTED_TOKEN_HEX_LEN: usize = REDACTED_TOKEN_HEX_BYTES * 2;
const REDACTED_TOKEN_LEN: usize = REDACTED_TOKEN_PREFIX.len() + REDACTED_TOKEN_HEX_LEN;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Categories of data that adapters may attempt to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDataClass {
    RawQueryText,
    EvidenceText,
    FilesystemPath,
    Identifier,
    AclValue,
    Token,
    SourceLabel,
    TenantLabel,
    BackendAttribute,
    RedactedTelemetryId,
    NumericCounter,
    DiagnosticCode,
}

/// Default telemetry destinations subject to the privacy policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDestination {
    DefaultMetric,
    SpanAttribute,
    ControlledRunArtifact,
}

/// The immutable default telemetry privacy policy.
///
/// It permanently refuses raw queries, evidence, filesystem paths, identifiers,
/// ACL values, tokens, and unbounded source/tenant labels. A later adapter that
/// needs sensitive diagnostic packs must define a separate access-controlled
/// boundary instead of weakening this contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPolicy {
    #[default]
    StrictDefault,
}

impl PrivacyPolicy {
    /// Returns the only policy available from this pure default contract.
    pub const fn strict_default() -> Self {
        Self::StrictDefault
    }

    /// Returns whether the closed data class is allowed at this destination.
    pub const fn permits(
        self,
        destination: TelemetryDestination,
        class: TelemetryDataClass,
    ) -> bool {
        match class {
            TelemetryDataClass::RawQueryText
            | TelemetryDataClass::EvidenceText
            | TelemetryDataClass::FilesystemPath
            | TelemetryDataClass::Identifier
            | TelemetryDataClass::AclValue
            | TelemetryDataClass::Token
            | TelemetryDataClass::SourceLabel
            | TelemetryDataClass::TenantLabel => false,
            TelemetryDataClass::BackendAttribute | TelemetryDataClass::RedactedTelemetryId => {
                !matches!(destination, TelemetryDestination::DefaultMetric)
            }
            TelemetryDataClass::NumericCounter | TelemetryDataClass::DiagnosticCode => true,
        }
    }

    /// Fails closed without retaining the forbidden field's value.
    pub fn validate_emission(
        self,
        destination: TelemetryDestination,
        class: TelemetryDataClass,
    ) -> TelemetryResult<()> {
        if self.permits(destination, class) {
            Ok(())
        } else {
            Err(TelemetryError::contract(
                TelemetryDiagnosticCode::PrivacyPolicyViolation,
            ))
        }
    }
}

/// A bounded token derived from raw trace/run input without retaining that input.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RedactedTelemetryId(String);

impl<'de> Deserialize<'de> for RedactedTelemetryId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let token = String::deserialize(deserializer)?;
        Self::from_opaque_token(token).map_err(serde::de::Error::custom)
    }
}

impl RedactedTelemetryId {
    /// Derives a stable opaque token from a bounded raw correlation value.
    ///
    /// The source string is never stored, exposed, or rendered by this type.
    pub fn new(source: impl AsRef<str>) -> TelemetryResult<Self> {
        let source = source.as_ref();
        if source.is_empty() || source.len() > MAX_REDACTED_TELEMETRY_ID_SOURCE_BYTES {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidRedactedTelemetryId,
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"verbatim.retrieval-telemetry.id.v1\0");
        hasher.update(source.as_bytes());
        let digest = hasher.finalize();
        let mut token = String::with_capacity(REDACTED_TOKEN_LEN);
        token.push_str(REDACTED_TOKEN_PREFIX);
        for byte in digest.iter().take(REDACTED_TOKEN_HEX_BYTES) {
            token.push(char::from(HEX[(byte >> 4) as usize]));
            token.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        Self::from_opaque_token(token)
    }

    /// Rehydrates only a canonical opaque token, never an arbitrary identifier.
    pub fn from_opaque_token(token: impl Into<String>) -> TelemetryResult<Self> {
        let token = Self(token.into());
        token.validate()?;
        Ok(token)
    }

    /// Revalidates the fixed prefix, length, and lowercase hexadecimal digest.
    pub fn validate(&self) -> TelemetryResult<()> {
        let Some(hex) = self.0.strip_prefix(REDACTED_TOKEN_PREFIX) else {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidRedactedTelemetryId,
            ));
        };
        let valid = self.0.len() == REDACTED_TOKEN_LEN
            && hex.len() == REDACTED_TOKEN_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if !valid {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidRedactedTelemetryId,
            ));
        }
        Ok(())
    }

    /// Returns the safe, opaque token usable for correlation outside metric labels.
    pub fn as_opaque_token(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedTelemetryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RedactedTelemetryId({})", self.0)
    }
}

impl fmt::Display for RedactedTelemetryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
