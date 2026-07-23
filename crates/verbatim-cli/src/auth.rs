//! Authentication helpers for daemon HTTP clients.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use verbatim_core::config::{self, Config};

/// Resolve the daemon static-token credential, with the environment taking precedence.
pub(crate) fn daemon_auth_token() -> Option<String> {
    let environment_token = std::env::var("VERBATIM_AUTH_TOKEN").ok();
    let configured_token = if environment_token.is_some() {
        String::new()
    } else {
        let path = config::config_path();
        if path.exists() {
            Config::load_from(&path)
                .ok()
                .map(|config| config.daemon.auth.static_token)
                .unwrap_or_default()
        } else {
            String::new()
        }
    };
    select_auth_token(environment_token, configured_token)
}

/// Prefer the environment token and discard empty credentials.
pub(crate) fn select_auth_token(
    environment_token: Option<String>,
    configured_token: String,
) -> Option<String> {
    environment_token
        .or_else(|| (!configured_token.is_empty()).then_some(configured_token))
        .filter(|token| !token.is_empty())
}

/// Build a daemon client that applies the resolved bearer token to every request.
pub(crate) fn daemon_client() -> Client {
    client_with_token(daemon_auth_token())
}

fn client_with_token(auth_token: Option<String>) -> Client {
    Client::builder()
        .default_headers(auth_headers(auth_token.as_deref()))
        .build()
        .expect("daemon HTTP client construction must succeed")
}

fn auth_headers(auth_token: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = auth_token {
        let value = format!("Bearer {token}")
            .parse()
            .expect("daemon auth tokens must be valid HTTP header values");
        headers.insert(AUTHORIZATION, value);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_token_selection_prefers_environment_and_ignores_empty_values() {
        assert_eq!(
            select_auth_token(Some("environment-token".into()), "config-token".into()),
            Some("environment-token".into())
        );
        assert_eq!(
            select_auth_token(None, "config-token".into()),
            Some("config-token".into())
        );
        assert_eq!(select_auth_token(None, String::new()), None);
    }

    #[test]
    fn daemon_client_headers_include_the_bearer_token() {
        let token = ["fixture", "token"].join("-");
        let headers = auth_headers(Some(&token));
        let authorization = headers
            .get(AUTHORIZATION)
            .expect("authorization header is present")
            .to_str()
            .expect("authorization header is valid text");

        assert_eq!(authorization, format!("Bearer {token}"));
    }
}
