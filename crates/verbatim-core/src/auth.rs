//! Shared identities, roles, and daemon authentication configuration.

use serde::{Deserialize, Serialize};

/// Authenticated identity for an inbound daemon request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// Loopback caller with no explicit authentication and full access.
    LocalAnonymous,
    /// Bearer-token authenticated caller with an assigned role.
    Token { role: Role },
}

/// Coarse-grained authorization role for the daemon MVP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Read-only access to search, retrieve, ask, list, status, and evidence endpoints.
    Reader,
    /// Read and write access to ingestion, reindexing, collections, and task submission.
    Editor,
    /// Full access including destructive operations.
    #[default]
    Admin,
}

/// Authentication mode for daemon HTTP requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    /// Allow loopback callers without a bearer token.
    #[default]
    LocalAnonymous,
    /// Require a configured static bearer token for every non-health request.
    StaticToken,
}

/// Daemon authentication configuration shared by the daemon and CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonAuthConfig {
    /// Authentication mode. Defaults to local-anonymous loopback access.
    #[serde(default)]
    pub mode: AuthMode,
    /// Static bearer token used by static-token mode.
    #[serde(default)]
    pub static_token: String,
    /// Permit sending static bearer tokens over an unencrypted non-loopback bind.
    #[serde(default)]
    pub allow_insecure_transport: bool,
    /// Role granted to callers authenticated with the static bearer token.
    #[serde(default)]
    pub static_token_role: Role,
}

#[cfg(test)]
mod tests {
    use super::{AuthMode, DaemonAuthConfig, Principal, Role};
    use crate::config::Config;

    #[test]
    fn auth_types_use_kebab_case_defaults() {
        let auth = DaemonAuthConfig::default();

        assert_eq!(auth.mode, AuthMode::LocalAnonymous);
        assert!(!auth.allow_insecure_transport);
        assert_eq!(auth.static_token_role, Role::Admin);
        let static_token: DaemonAuthConfig = toml::from_str(
            "mode = \"static-token\"\nallow_insecure_transport = true\nstatic_token_role = \"reader\"",
        )
        .unwrap();
        assert_eq!(static_token.mode, AuthMode::StaticToken);
        assert!(static_token.allow_insecure_transport);
        assert_eq!(static_token.static_token_role, Role::Reader);
        assert!(matches!(
            Principal::LocalAnonymous,
            Principal::LocalAnonymous
        ));
    }

    #[test]
    fn daemon_auth_config_defaults_and_parses_static_token_mode() {
        let default_config = Config::default();
        assert_eq!(default_config.daemon.auth, DaemonAuthConfig::default());

        let config: Config = toml::from_str(
            "[daemon.auth]\nmode = \"static-token\"\nstatic_token = \"fixture-token\"\nallow_insecure_transport = true\nstatic_token_role = \"editor\"",
        )
        .unwrap();
        assert_eq!(config.daemon.auth.mode, AuthMode::StaticToken);
        assert_eq!(config.daemon.auth.static_token, "fixture-token");
        assert!(config.daemon.auth.allow_insecure_transport);
        assert_eq!(config.daemon.auth.static_token_role, Role::Editor);
    }
}
