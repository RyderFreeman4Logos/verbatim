//! Protocol/schema compatibility negotiation (fail closed).

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageResult};

/// Wire schema version for remote client documents in this crate module.
pub const REMOTE_STORAGE_CLIENT_SCHEMA_VERSION: u32 = 1;

/// Protocol major version offered by this client walking skeleton.
pub const REMOTE_STORAGE_CLIENT_PROTOCOL_VERSION: u32 = 1;

/// Semantic-ish protocol version (`major` only for the walking skeleton).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u32,
}

impl ProtocolVersion {
    pub const fn new(major: u32) -> Self {
        Self { major }
    }

    pub fn current() -> Self {
        Self::new(REMOTE_STORAGE_CLIENT_PROTOCOL_VERSION)
    }
}

/// Document schema version for negotiated envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub major: u32,
}

impl SchemaVersion {
    pub const fn new(major: u32) -> Self {
        Self { major }
    }

    pub fn current() -> Self {
        Self::new(REMOTE_STORAGE_CLIENT_SCHEMA_VERSION)
    }
}

/// Inclusive compatibility window for protocol/schema majors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompatibilityWindow {
    pub min_protocol: ProtocolVersion,
    pub max_protocol: ProtocolVersion,
    pub min_schema: SchemaVersion,
    pub max_schema: SchemaVersion,
}

impl CompatibilityWindow {
    pub fn current_only() -> Self {
        Self {
            min_protocol: ProtocolVersion::current(),
            max_protocol: ProtocolVersion::current(),
            min_schema: SchemaVersion::current(),
            max_schema: SchemaVersion::current(),
        }
    }

    pub fn validate(self) -> StorageResult<()> {
        if self.min_protocol.major == 0 || self.max_protocol.major == 0 {
            return Err(StorageError::invalid_request(
                "protocol version major must be > 0",
            ));
        }
        if self.min_schema.major == 0 || self.max_schema.major == 0 {
            return Err(StorageError::invalid_request(
                "schema version major must be > 0",
            ));
        }
        if self.min_protocol > self.max_protocol {
            return Err(StorageError::invalid_request(
                "compatibility window min_protocol exceeds max_protocol",
            ));
        }
        if self.min_schema > self.max_schema {
            return Err(StorageError::invalid_request(
                "compatibility window min_schema exceeds max_schema",
            ));
        }
        Ok(())
    }

    pub fn supports_protocol(self, version: ProtocolVersion) -> bool {
        version >= self.min_protocol && version <= self.max_protocol
    }

    pub fn supports_schema(self, version: SchemaVersion) -> bool {
        version >= self.min_schema && version <= self.max_schema
    }
}

/// Offer sent during connection / request handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityOffer {
    pub schema_version: u32,
    pub protocol: ProtocolVersion,
    pub document_schema: SchemaVersion,
    pub window: CompatibilityWindow,
}

impl CompatibilityOffer {
    pub fn current() -> Self {
        Self {
            schema_version: REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
            protocol: ProtocolVersion::current(),
            document_schema: SchemaVersion::current(),
            window: CompatibilityWindow::current_only(),
        }
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.schema_version != REMOTE_STORAGE_CLIENT_SCHEMA_VERSION {
            return Err(StorageError::invalid_request(format!(
                "unsupported remote storage client schema version {}; expected {REMOTE_STORAGE_CLIENT_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        self.window.validate()?;
        if !self.window.supports_protocol(self.protocol) {
            return Err(StorageError::invalid_request(
                "offered protocol is outside the declared compatibility window",
            ));
        }
        if !self.window.supports_schema(self.document_schema) {
            return Err(StorageError::invalid_request(
                "offered document schema is outside the declared compatibility window",
            ));
        }
        Ok(())
    }

    /// Negotiate with a peer offer. Fail closed when no overlap exists.
    pub fn negotiate(&self, peer: &Self) -> StorageResult<NegotiatedCompatibility> {
        self.validate()?;
        peer.validate()?;

        let protocol = intersect_protocol(self, peer)?;
        let document_schema = intersect_schema(self, peer)?;
        Ok(NegotiatedCompatibility {
            protocol,
            document_schema,
        })
    }
}

/// Successfully negotiated protocol + document schema majors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NegotiatedCompatibility {
    pub protocol: ProtocolVersion,
    pub document_schema: SchemaVersion,
}

fn intersect_protocol(
    local: &CompatibilityOffer,
    peer: &CompatibilityOffer,
) -> StorageResult<ProtocolVersion> {
    let min = local.window.min_protocol.max(peer.window.min_protocol);
    let max = local.window.max_protocol.min(peer.window.max_protocol);
    if min > max {
        return Err(StorageError::unsupported(
            crate::storage_ports::StorageCapabilityKind::CatalogStore,
            format!(
                "protocol versions incompatible: local {}-{} peer {}-{}",
                local.window.min_protocol.major,
                local.window.max_protocol.major,
                peer.window.min_protocol.major,
                peer.window.max_protocol.major
            ),
        ));
    }
    // Prefer highest mutually supported major.
    Ok(max)
}

fn intersect_schema(
    local: &CompatibilityOffer,
    peer: &CompatibilityOffer,
) -> StorageResult<SchemaVersion> {
    let min = local.window.min_schema.max(peer.window.min_schema);
    let max = local.window.max_schema.min(peer.window.max_schema);
    if min > max {
        return Err(StorageError::unsupported(
            crate::storage_ports::StorageCapabilityKind::CatalogStore,
            format!(
                "schema versions incompatible: local {}-{} peer {}-{}",
                local.window.min_schema.major,
                local.window.max_schema.major,
                peer.window.min_schema.major,
                peer.window.max_schema.major
            ),
        ));
    }
    Ok(max)
}

/// Decode a compatibility offer — fail closed on unknown schema versions.
pub fn decode_compatibility_offer_json(bytes: &[u8]) -> StorageResult<CompatibilityOffer> {
    let value: CompatibilityOffer = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("compatibility offer decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
