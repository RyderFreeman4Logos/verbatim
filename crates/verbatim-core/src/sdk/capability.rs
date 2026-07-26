//! Capability discovery, cache, and graceful unsupported negotiation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::error::{ClientError, ClientResult};

/// Wire schema version for SDK capability descriptors.
pub const SDK_CAPABILITY_SCHEMA_VERSION: u32 = 1;

/// Named public API capability classes exposed to SDK clients.
///
/// Distinct from [`crate::storage_ports::StorageCapabilityKind`]: these name
/// *client-facing* R/A/G and workflow operations, not storage backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkCapabilityKind {
    Capabilities,
    SourceUpload,
    Search,
    Retrieve,
    Resolve,
    Evidence,
    Context,
    Generate,
    Verify,
    Workflow,
    Task,
    Artifact,
}

impl SdkCapabilityKind {
    pub const ALL: [Self; 12] = [
        Self::Capabilities,
        Self::SourceUpload,
        Self::Search,
        Self::Retrieve,
        Self::Resolve,
        Self::Evidence,
        Self::Context,
        Self::Generate,
        Self::Verify,
        Self::Workflow,
        Self::Task,
        Self::Artifact,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::SourceUpload => "source_upload",
            Self::Search => "search",
            Self::Retrieve => "retrieve",
            Self::Resolve => "resolve",
            Self::Evidence => "evidence",
            Self::Context => "context",
            Self::Generate => "generate",
            Self::Verify => "verify",
            Self::Workflow => "workflow",
            Self::Task => "task",
            Self::Artifact => "artifact",
        }
    }
}

/// Capability descriptor returned by discovery / negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkCapabilityDescriptor {
    pub schema_version: u32,
    pub capabilities: BTreeSet<SdkCapabilityKind>,
    /// Optional negotiated protocol major (walking skeleton: always 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_major: Option<u32>,
    /// Optional negotiated wire schema major (API-002).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_schema_major: Option<u32>,
    /// Optional human-readable server / profile label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_label: Option<String>,
}

impl SdkCapabilityDescriptor {
    pub fn new(capabilities: impl IntoIterator<Item = SdkCapabilityKind>) -> Self {
        Self {
            schema_version: SDK_CAPABILITY_SCHEMA_VERSION,
            capabilities: capabilities.into_iter().collect(),
            protocol_major: Some(1),
            wire_schema_major: Some(1),
            server_label: None,
        }
    }

    pub fn all_supported() -> Self {
        Self::new(SdkCapabilityKind::ALL)
    }

    pub fn with_server_label(mut self, label: impl Into<String>) -> ClientResult<Self> {
        let label = label.into();
        if label.trim().is_empty() {
            return Err(ClientError::validation(
                "server_label must not be empty when set",
            ));
        }
        self.server_label = Some(label);
        Ok(self)
    }

    pub fn supports(&self, capability: SdkCapabilityKind) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn validate(&self) -> ClientResult<()> {
        if self.schema_version != SDK_CAPABILITY_SCHEMA_VERSION {
            return Err(ClientError::compatibility(format!(
                "unsupported sdk capability schema version {}; expected {SDK_CAPABILITY_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if let Some(label) = &self.server_label {
            if label.trim().is_empty() {
                return Err(ClientError::validation(
                    "server_label must not be empty when set",
                ));
            }
        }
        if let Some(major) = self.protocol_major {
            if major == 0 {
                return Err(ClientError::compatibility(
                    "protocol_major must be > 0 when set",
                ));
            }
        }
        if let Some(major) = self.wire_schema_major {
            if major == 0 {
                return Err(ClientError::compatibility(
                    "wire_schema_major must be > 0 when set",
                ));
            }
        }
        Ok(())
    }
}

/// Client-side cache of the last successful capability negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilityCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<SdkCapabilityDescriptor>,
    /// Unix epoch seconds when the cache was last refreshed (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refreshed_at_unix: Option<u64>,
}

impl CapabilityCache {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_descriptor(descriptor: SdkCapabilityDescriptor) -> ClientResult<Self> {
        descriptor.validate()?;
        Ok(Self {
            descriptor: Some(descriptor),
            refreshed_at_unix: None,
        })
    }

    pub fn with_refreshed_at(mut self, unix: u64) -> Self {
        self.refreshed_at_unix = Some(unix);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.descriptor.is_none()
    }

    pub fn supports(&self, capability: SdkCapabilityKind) -> Option<bool> {
        self.descriptor.as_ref().map(|d| d.supports(capability))
    }

    pub fn validate(&self) -> ClientResult<()> {
        if let Some(descriptor) = &self.descriptor {
            descriptor.validate()?;
        }
        Ok(())
    }
}

/// Capability negotiation helper: feature discovery + graceful unsupported.
///
/// Holds the local required set and the remote advertised set, then either
/// succeeds with an intersection or fails closed with typed
/// [`ClientError::Unsupported`] / [`ClientError::Compatibility`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityNegotiation {
    pub required: BTreeSet<SdkCapabilityKind>,
    pub advertised: SdkCapabilityDescriptor,
}

impl CapabilityNegotiation {
    pub fn new(
        required: impl IntoIterator<Item = SdkCapabilityKind>,
        advertised: SdkCapabilityDescriptor,
    ) -> ClientResult<Self> {
        advertised.validate()?;
        Ok(Self {
            required: required.into_iter().collect(),
            advertised,
        })
    }

    /// Intersect required ∩ advertised. Fails if any required capability is
    /// missing or if the advertised descriptor is invalid.
    pub fn negotiate(&self) -> ClientResult<SdkCapabilityDescriptor> {
        self.advertised.validate()?;
        let mut missing = Vec::new();
        for cap in &self.required {
            if !self.advertised.supports(*cap) {
                missing.push(*cap);
            }
        }
        if let Some(first) = missing.first().copied() {
            let names: Vec<&str> = missing.iter().map(|c| c.as_str()).collect();
            return Err(ClientError::unsupported(
                first,
                "capability_negotiation",
                format!("missing required capabilities: {}", names.join(", ")),
            ));
        }
        // Prefer the advertised set filtered to required when required is
        // non-empty; otherwise return the full advertised descriptor.
        if self.required.is_empty() {
            return Ok(self.advertised.clone());
        }
        let mut negotiated = self.advertised.clone();
        negotiated.capabilities = self.required.clone();
        negotiated.validate()?;
        Ok(negotiated)
    }

    /// Soft check used before calling an operation: maps missing capability to
    /// typed unsupported without performing transport.
    pub fn require(&self, capability: SdkCapabilityKind, operation: &str) -> ClientResult<()> {
        self.advertised.validate()?;
        if self.advertised.supports(capability) {
            Ok(())
        } else {
            Err(ClientError::unsupported(
                capability,
                operation,
                format!(
                    "capability {} is not advertised by the server",
                    capability.as_str()
                ),
            ))
        }
    }
}

/// Decode a capability descriptor from compact JSON; fail closed on bad schema.
pub fn decode_sdk_capability_descriptor_json(
    bytes: &[u8],
) -> ClientResult<SdkCapabilityDescriptor> {
    let value: SdkCapabilityDescriptor = serde_json::from_slice(bytes).map_err(|err| {
        ClientError::validation(format!("invalid sdk capability descriptor json: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
