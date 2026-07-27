//! Exact, stable ADK-Rust version policy.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::adk_integration::{
    AdkIntegrationDiagnosticCode, AdkIntegrationError, AdkIntegrationResult,
};

/// A parsed exact semantic version accepted by the ADK integration boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdkVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl AdkVersion {
    fn parse_exact(value: &str) -> AdkIntegrationResult<Self> {
        let mut components = value.split('.');
        let Some(major) = components.next() else {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        };
        let Some(minor) = components.next() else {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        };
        let Some(patch) = components.next() else {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        };
        if components.next().is_some() {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        }

        let major = Self::parse_component(major)?;
        let minor = Self::parse_component(minor)?;
        let patch = Self::parse_component(patch)?;
        if major != 1 {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        }

        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn parse_component(component: &str) -> AdkIntegrationResult<u64> {
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ));
        }
        component.parse::<u64>().map_err(|_| {
            AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            )
        })
    }

    pub const fn major(self) -> u64 {
        self.major
    }

    pub const fn minor(self) -> u64 {
        self.minor
    }

    pub const fn patch(self) -> u64 {
        self.patch
    }
}

impl fmt::Display for AdkVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Policy requiring an exact, stable ADK-Rust 1.x release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionPolicy {
    version: AdkVersion,
}

impl VersionPolicy {
    pub fn new(exact_version: &str) -> AdkIntegrationResult<Self> {
        Ok(Self {
            version: AdkVersion::parse_exact(exact_version)?,
        })
    }

    pub const fn version(self) -> AdkVersion {
        self.version
    }

    pub(crate) fn validate(self) -> AdkIntegrationResult<()> {
        if self.version.major() == 1 {
            Ok(())
        } else {
            Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::VersionMustBeExactStableOneX,
            ))
        }
    }
}

impl fmt::Display for VersionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.version.fmt(formatter)
    }
}

/// Encodes a validated version policy without leaking serialization details.
pub fn encode_version_policy_json(policy: &VersionPolicy) -> AdkIntegrationResult<String> {
    policy.validate()?;
    serde_json::to_string(policy).map_err(|_| {
        AdkIntegrationError::validation(
            AdkIntegrationDiagnosticCode::VersionPolicySerializationFailed,
        )
    })
}

/// Decodes and revalidates a version policy at the serialization boundary.
pub fn decode_version_policy_json(input: &str) -> AdkIntegrationResult<VersionPolicy> {
    let policy: VersionPolicy = serde_json::from_str(input).map_err(|_| {
        AdkIntegrationError::validation(AdkIntegrationDiagnosticCode::InvalidVersionPolicyJson)
    })?;
    policy.validate()?;
    Ok(policy)
}
