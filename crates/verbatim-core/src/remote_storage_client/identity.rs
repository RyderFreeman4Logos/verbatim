//! Authenticated remote client identity and service principal.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageAuthContext, StorageError, StoragePrincipal, StorageResult};

/// Coarse service-to-service role for remote storage/index calls.
///
/// Distinct from daemon [`crate::auth::Role`]: this names the *service principal*
/// capability, not a human operator role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRole {
    /// Read-only remote caller (search, get, list, head).
    Reader,
    /// Mutating remote caller (put, upsert, enqueue, publish).
    Writer,
    /// Administrative remote caller (delete collections, force finish).
    Admin,
    /// Peer storage/index service acting on behalf of itself.
    ServicePeer,
}

impl ServiceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Admin => "admin",
            Self::ServicePeer => "service_peer",
        }
    }

    /// Whether this role may perform mutations under the client contract.
    pub fn may_mutate(self) -> bool {
        matches!(self, Self::Writer | Self::Admin | Self::ServicePeer)
    }
}

/// Wire-safe authenticated identity for a remote storage/index client.
///
/// Secrets (tokens, cert material) stay outside this type. Callers carry only
/// the resolved principal after authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteClientIdentity {
    pub schema_version: u32,
    pub principal: RemoteServicePrincipal,
    /// Optional ACL scope (collection / tenant / namespace). Never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl_scope: Option<String>,
    /// Stable client instance label for audit (not a credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_instance_id: Option<String>,
}

/// Resolved service principal after authentication (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteServicePrincipal {
    /// Unauthenticated — remote paths must fail closed as unauthorized.
    Unauthenticated,
    /// Named service principal with a role.
    Service {
        service_id: String,
        role: ServiceRole,
    },
    /// Human/operator token principal projected onto remote calls.
    Token { role: ServiceRole },
}

impl RemoteClientIdentity {
    pub fn unauthenticated() -> Self {
        Self {
            schema_version: super::REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
            principal: RemoteServicePrincipal::Unauthenticated,
            acl_scope: None,
            client_instance_id: None,
        }
    }

    pub fn service(service_id: impl Into<String>, role: ServiceRole) -> StorageResult<Self> {
        let service_id = service_id.into();
        if service_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "remote service_id must not be empty",
            ));
        }
        Ok(Self {
            schema_version: super::REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
            principal: RemoteServicePrincipal::Service { service_id, role },
            acl_scope: None,
            client_instance_id: None,
        })
    }

    pub fn token(role: ServiceRole) -> Self {
        Self {
            schema_version: super::REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
            principal: RemoteServicePrincipal::Token { role },
            acl_scope: None,
            client_instance_id: None,
        }
    }

    pub fn with_acl_scope(mut self, scope: impl Into<String>) -> Self {
        self.acl_scope = Some(scope.into());
        self
    }

    pub fn with_client_instance_id(mut self, id: impl Into<String>) -> Self {
        self.client_instance_id = Some(id.into());
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.schema_version != super::REMOTE_STORAGE_CLIENT_SCHEMA_VERSION {
            return Err(StorageError::invalid_request(format!(
                "unsupported remote storage client schema version {}; expected {}",
                self.schema_version,
                super::REMOTE_STORAGE_CLIENT_SCHEMA_VERSION
            )));
        }
        match &self.principal {
            RemoteServicePrincipal::Unauthenticated => {}
            RemoteServicePrincipal::Service { service_id, .. } => {
                if service_id.trim().is_empty() {
                    return Err(StorageError::invalid_request(
                        "remote service_id must not be empty",
                    ));
                }
            }
            RemoteServicePrincipal::Token { .. } => {}
        }
        if let Some(scope) = &self.acl_scope {
            if scope.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "remote acl_scope must not be empty when set",
                ));
            }
        }
        if let Some(id) = &self.client_instance_id {
            if id.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "remote client_instance_id must not be empty when set",
                ));
            }
        }
        Ok(())
    }

    /// True when the identity is not authenticated — remote clients must refuse
    /// enumeration and fetch before any network hop.
    pub fn is_unauthenticated(&self) -> bool {
        matches!(self.principal, RemoteServicePrincipal::Unauthenticated)
    }

    /// Fail closed: unauthenticated identities cannot enumerate or fetch.
    pub fn require_authenticated(&self, operation: &str) -> StorageResult<()> {
        self.validate()?;
        if self.is_unauthenticated() {
            return Err(StorageError::unauthorized(format!(
                "unauthenticated remote client cannot {operation}"
            )));
        }
        Ok(())
    }

    /// Fail closed for mutations under reader-only roles.
    pub fn require_mutation(&self, operation: &str) -> StorageResult<()> {
        self.require_authenticated(operation)?;
        let role = match &self.principal {
            RemoteServicePrincipal::Unauthenticated => unreachable!("checked above"),
            RemoteServicePrincipal::Service { role, .. }
            | RemoteServicePrincipal::Token { role } => *role,
        };
        if !role.may_mutate() {
            return Err(StorageError::unauthorized(format!(
                "remote role {} cannot {operation}",
                role.as_str()
            )));
        }
        Ok(())
    }

    /// Project onto a [`StorageAuthContext`] for port-level calls.
    pub fn to_storage_auth(&self) -> StorageResult<StorageAuthContext> {
        self.validate()?;
        let principal = match &self.principal {
            RemoteServicePrincipal::Unauthenticated => {
                return Err(StorageError::unauthorized(
                    "cannot project unauthenticated remote identity to storage auth",
                ));
            }
            RemoteServicePrincipal::Service { role, .. }
            | RemoteServicePrincipal::Token { role } => StoragePrincipal::Token {
                role: role.as_str().to_string(),
            },
        };
        let mut auth = StorageAuthContext::new(principal);
        if let Some(scope) = &self.acl_scope {
            auth = auth.with_acl_scope(scope.clone());
        }
        auth.validate()?;
        Ok(auth)
    }
}
