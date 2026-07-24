//! Authentication and route-level authorization middleware for the daemon.

use std::net::{IpAddr, SocketAddr};

use anyhow::{bail, Result};
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use verbatim_core::config::DaemonConfig;
use verbatim_core::{AuthMode, DaemonAuthConfig, Principal, Role};

const HEALTH_PATH: &str = "/api/health";

/// Reject anonymously exposed daemon binds before the server begins accepting requests.
pub(super) fn validate_daemon_auth_bind(daemon: &DaemonConfig) -> Result<()> {
    if matches!(daemon.auth.mode, AuthMode::LocalAnonymous) && !is_loopback_bind(&daemon.bind) {
        bail!(
            "refusing to start daemon with daemon.auth.mode = \"local-anonymous\" on non-loopback bind {}; configure static-token authentication before exposing the daemon",
            daemon.bind
        );
    }
    if matches!(daemon.auth.mode, AuthMode::StaticToken)
        && !is_loopback_bind(&daemon.bind)
        && !daemon.auth.allow_insecure_transport
    {
        bail!(
            "refusing to start daemon with daemon.auth.mode = \"static-token\" on non-loopback bind {}; static bearer tokens require encrypted transport, set daemon.auth.allow_insecure_transport = true only for an explicitly trusted network",
            daemon.bind
        );
    }
    Ok(())
}

fn is_loopback_bind(bind: &str) -> bool {
    if let Ok(address) = bind.parse::<SocketAddr>() {
        return address.ip().is_loopback();
    }

    let Some((host, _port)) = bind.rsplit_once(':') else {
        return false;
    };
    if host.contains(':') {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    host == "localhost"
}

/// Immutable authentication data captured when the daemon router starts.
#[derive(Clone)]
pub(super) struct AuthMiddlewareState {
    mode: AuthMode,
    static_token: String,
    static_token_role: Role,
}

impl AuthMiddlewareState {
    /// Build middleware state after resolving the process environment override.
    pub(super) fn from_runtime_config(config: DaemonAuthConfig) -> Self {
        let static_token = resolve_static_token(
            std::env::var("VERBATIM_AUTH_TOKEN").ok(),
            config.static_token,
        );
        Self {
            mode: config.mode,
            static_token,
            static_token_role: config.static_token_role,
        }
    }

    #[cfg(test)]
    fn from_config(config: DaemonAuthConfig) -> Self {
        Self {
            mode: config.mode,
            static_token: config.static_token,
            static_token_role: config.static_token_role,
        }
    }
}

/// Authenticate an incoming request, attach its principal, and enforce its route role.
pub(super) async fn authenticate_request(
    State(state): State<AuthMiddlewareState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == HEALTH_PATH {
        return next.run(request).await;
    }

    let principal = match state.mode {
        AuthMode::LocalAnonymous => match request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(address)| address.ip().is_loopback())
        {
            Some(true) => Principal::LocalAnonymous,
            Some(false) | None => {
                return forbidden_response("local-anonymous mode only permits loopback callers")
            }
        },
        AuthMode::StaticToken => match bearer_token(&request) {
            Some(token) if constant_time_eq(token.as_bytes(), state.static_token.as_bytes()) => {
                Principal::Token {
                    role: state.static_token_role,
                }
            }
            _ => return unauthorized_response(),
        },
    };

    if let Some(required_role) = endpoint_required_role(request.method(), request.uri().path()) {
        if require_role(&principal, required_role).is_err() {
            return forbidden_response("authenticated principal lacks the required role");
        }
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// Return whether a principal has the requested coarse-grained role.
pub(super) fn require_role(principal: &Principal, required: Role) -> Result<(), StatusCode> {
    let granted = match principal {
        Principal::LocalAnonymous => Role::Admin,
        Principal::Token { role } => *role,
    };
    (role_level(granted) >= role_level(required))
        .then_some(())
        .ok_or(StatusCode::FORBIDDEN)
}

/// Classify a daemon endpoint by the minimum role required for the MVP.
pub(super) fn endpoint_required_role(method: &Method, path: &str) -> Option<Role> {
    if path == HEALTH_PATH {
        return None;
    }

    match (method, path) {
        (&Method::DELETE, path)
            if path.starts_with("/api/sources/") || path.starts_with("/api/collections/") =>
        {
            Some(Role::Admin)
        }
        (&Method::POST, "/api/index/gc")
        | (&Method::POST, "/api/index/profiles/delete")
        | (&Method::POST, "/api/index/vector-json/cleanup") => Some(Role::Admin),
        (&Method::GET, "/api/config")
        | (&Method::GET, "/api/sources")
        | (&Method::GET, "/api/deletions/reports")
        | (&Method::GET, "/api/collections")
        | (&Method::GET, "/api/index/status")
        | (&Method::POST, "/api/retrieve")
        | (&Method::POST, "/api/ask")
        | (&Method::POST, "/api/ask/stream")
        | (&Method::POST, "/api/sources/check") => Some(Role::Reader),
        (&Method::GET, path)
            if path.starts_with("/api/sources/")
                || path.starts_with("/api/collections/")
                || path.starts_with("/api/tasks")
                || path.starts_with("/api/evidence/") =>
        {
            Some(Role::Reader)
        }
        (&Method::POST, "/api/sources")
        | (&Method::POST, "/api/reindex")
        | (&Method::POST, "/api/collections") => Some(Role::Editor),
        (&Method::POST, path)
            if path.starts_with("/api/ingest")
                || path.starts_with("/api/collections/")
                || path.starts_with("/api/tasks/") =>
        {
            Some(Role::Editor)
        }
        (&Method::PUT, path) if path.starts_with("/api/collections/") => Some(Role::Editor),
        _ => None,
    }
}

fn bearer_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
}

fn role_level(role: Role) -> u8 {
    match role {
        Role::Reader => 0,
        Role::Editor => 1,
        Role::Admin => 2,
    }
}

fn resolve_static_token(environment_token: Option<String>, configured_token: String) -> String {
    environment_token.unwrap_or(configured_token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_length = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..max_length {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn unauthorized_response() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or invalid Bearer token").into_response()
}

fn forbidden_response(message: &'static str) -> Response {
    (StatusCode::FORBIDDEN, message).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::ConnectInfo;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::middleware;
    use axum::routing::{get, post};
    use axum::{Extension, Router};
    use std::net::SocketAddr;
    use tower::ServiceExt;
    use verbatim_core::{AuthMode, DaemonAuthConfig, Principal, Role};

    fn auth_router(config: DaemonAuthConfig) -> Router {
        Router::new()
            .route("/api/health", get(|| async { StatusCode::OK }))
            .route(
                "/api/config",
                get(|Extension(_principal): Extension<Principal>| async { StatusCode::OK }),
            )
            .route("/api/index/gc", post(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn_with_state(
                AuthMiddlewareState::from_config(config),
                authenticate_request,
            ))
    }

    fn request(method: Method, path: &str, token: Option<&str>, remote: &str) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = builder.body(Body::empty()).unwrap();
        let remote = remote.parse::<SocketAddr>().unwrap();
        request.extensions_mut().insert(ConnectInfo(remote));
        request
    }

    #[test]
    fn local_anonymous_rejects_non_loopback_bind_addresses() {
        let mut config = verbatim_core::config::DaemonConfig::default();
        config.bind = "0.0.0.0:7700".into();

        let error = validate_daemon_auth_bind(&config).unwrap_err();
        assert!(error
            .to_string()
            .contains("daemon.auth.mode = \"local-anonymous\""));
        assert!(error.to_string().contains("0.0.0.0:7700"));

        for bind in [
            "127.0.0.1:7700",
            "127.0.0.2:7700",
            "localhost:7700",
            "[::1]:7700",
        ] {
            config.bind = bind.into();
            assert!(
                validate_daemon_auth_bind(&config).is_ok(),
                "{bind} must be loopback"
            );
        }

        config.bind = "::1:7700".into();
        assert!(
            validate_daemon_auth_bind(&config).is_err(),
            "unbracketed IPv6 socket addresses must not be accepted"
        );

        for bind in ["127.0.0.1.example:7700", "localhost.evil:7700"] {
            config.bind = bind.into();
            assert!(
                validate_daemon_auth_bind(&config).is_err(),
                "{bind} must not be treated as loopback"
            );
        }

        config.bind = "0.0.0.0:7700".into();
        config.auth.mode = AuthMode::StaticToken;
        let error = validate_daemon_auth_bind(&config).unwrap_err();
        assert!(error
            .to_string()
            .contains("allow_insecure_transport = true"));

        config.auth.allow_insecure_transport = true;
        assert!(validate_daemon_auth_bind(&config).is_ok());
    }

    #[tokio::test]
    async fn local_anonymous_permits_loopback_and_rejects_non_loopback() {
        let loopback = auth_router(DaemonAuthConfig::default())
            .oneshot(request(Method::GET, "/api/config", None, "127.0.0.1:43210"))
            .await
            .unwrap();
        assert_eq!(loopback.status(), StatusCode::OK);

        let non_loopback = auth_router(DaemonAuthConfig::default())
            .oneshot(request(Method::GET, "/api/config", None, "192.0.2.1:43210"))
            .await
            .unwrap();
        assert_eq!(non_loopback.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn static_token_requires_an_exact_bearer_token() {
        let config = DaemonAuthConfig {
            mode: AuthMode::StaticToken,
            static_token: "fixture-token".into(),
            allow_insecure_transport: false,
            static_token_role: Role::Reader,
        };

        let valid = auth_router(config.clone())
            .oneshot(request(
                Method::GET,
                "/api/config",
                Some("fixture-token"),
                "192.0.2.1:43210",
            ))
            .await
            .unwrap();
        assert_eq!(valid.status(), StatusCode::OK);

        for token in [None, Some("wrong-token"), Some("fixture-token-suffix")] {
            let response = auth_router(config.clone())
                .oneshot(request(
                    Method::GET,
                    "/api/config",
                    token,
                    "192.0.2.1:43210",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn health_is_exempt_from_authentication() {
        let response = auth_router(DaemonAuthConfig {
            mode: AuthMode::StaticToken,
            static_token: "fixture-token".into(),
            allow_insecure_transport: false,
            static_token_role: Role::Admin,
        })
        .oneshot(request(Method::GET, "/api/health", None, "192.0.2.1:43210"))
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn static_token_role_restricts_destructive_routes() {
        let response = auth_router(DaemonAuthConfig {
            mode: AuthMode::StaticToken,
            static_token: "fixture-token".into(),
            allow_insecure_transport: false,
            static_token_role: Role::Editor,
        })
        .oneshot(request(
            Method::POST,
            "/api/index/gc",
            Some("fixture-token"),
            "192.0.2.1:43210",
        ))
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn role_checks_enforce_the_role_hierarchy() {
        for required in [Role::Reader, Role::Editor, Role::Admin] {
            assert_eq!(require_role(&Principal::LocalAnonymous, required), Ok(()));
        }
        assert_eq!(
            require_role(&Principal::Token { role: Role::Reader }, Role::Reader),
            Ok(())
        );
        assert_eq!(
            require_role(&Principal::Token { role: Role::Reader }, Role::Editor),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            require_role(&Principal::Token { role: Role::Editor }, Role::Reader),
            Ok(())
        );
        assert_eq!(
            require_role(&Principal::Token { role: Role::Editor }, Role::Admin),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            require_role(&Principal::Token { role: Role::Admin }, Role::Admin),
            Ok(())
        );
    }

    #[test]
    fn endpoint_roles_match_the_mvp_route_categories() {
        assert_eq!(endpoint_required_role(&Method::GET, "/api/health"), None);
        assert_eq!(
            endpoint_required_role(&Method::GET, "/api/config"),
            Some(Role::Reader)
        );
        assert_eq!(
            endpoint_required_role(&Method::GET, "/api/sources/source-1"),
            Some(Role::Reader)
        );
        assert_eq!(
            endpoint_required_role(&Method::POST, "/api/ask/stream"),
            Some(Role::Reader)
        );
        assert_eq!(
            endpoint_required_role(&Method::POST, "/api/sources"),
            Some(Role::Editor)
        );
        assert_eq!(
            endpoint_required_role(&Method::POST, "/api/ingest/source-1"),
            Some(Role::Editor)
        );
        assert_eq!(
            endpoint_required_role(&Method::PUT, "/api/collections/articles"),
            Some(Role::Editor)
        );
        assert_eq!(
            endpoint_required_role(&Method::POST, "/api/tasks/task-1/cancel"),
            Some(Role::Editor)
        );
        assert_eq!(
            endpoint_required_role(&Method::DELETE, "/api/sources/source-1"),
            Some(Role::Admin)
        );
        assert_eq!(
            endpoint_required_role(&Method::DELETE, "/api/collections/articles"),
            Some(Role::Admin)
        );
        assert_eq!(
            endpoint_required_role(&Method::POST, "/api/index/profiles/delete"),
            Some(Role::Admin)
        );
        assert_eq!(endpoint_required_role(&Method::GET, "/api/unknown"), None);
    }
}
