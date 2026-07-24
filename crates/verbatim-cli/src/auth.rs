//! Authentication helpers for daemon HTTP clients.

use reqwest::blocking::{Client, RequestBuilder};
#[cfg(test)]
use reqwest::header::{HeaderMap, AUTHORIZATION};
use serde_json::Value;
use url::Host;
use verbatim_core::config::{self, Config};

pub(crate) const HTTP_ERROR_TRUNCATION_MARKER: &str = "...[truncated]";

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

/// Build a daemon HTTP client. Auth headers are applied per-request after
/// transport safety checks, not as client defaults.
pub(crate) fn daemon_client() -> Client {
    Client::new()
}

/// Check whether the given URL is safe for sending bearer tokens.
/// Safe means HTTPS, or loopback HTTP, or explicit insecure opt-in.
pub(crate) fn is_safe_transport(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };

    match parsed.scheme() {
        "https" => true,
        "http" => {
            let Some(host) = parsed.host() else {
                return false;
            };
            match host {
                Host::Ipv4(ip) => ip.is_loopback(),
                Host::Ipv6(ip) => ip.is_loopback(),
                Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
            }
        }
        _ => false,
    }
}

/// Read `allow_insecure_transport` from config (if available).
pub(crate) fn allow_insecure_transport() -> bool {
    let path = config::config_path();
    if !path.exists() {
        return false;
    }
    Config::load_from(&path)
        .map(|c| c.daemon.auth.allow_insecure_transport)
        .unwrap_or(false)
}

/// Construct an HTTP base URL from a daemon bind address.
pub(crate) fn bind_to_base_url(bind: &str) -> String {
    if bind.starts_with("http://") || bind.starts_with("https://") {
        bind.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", bind.trim_end_matches('/'))
    }
}

/// Apply the bearer token to a request if transport is safe.
/// Unsafe transports receive a warning and no Authorization header.
pub(crate) fn authorize_request(
    request: RequestBuilder,
    url: &str,
    token: Option<&str>,
) -> RequestBuilder {
    let Some(token) = token else {
        return request;
    };
    let safe = is_safe_transport(url) || allow_insecure_transport();
    if !safe {
        eprintln!(
            "warning: refusing to send bearer token over insecure transport to {url}; \
             set daemon.auth.allow_insecure_transport = true to override"
        );
        return request;
    }
    request.bearer_auth(token)
}

/// Redact credentials from an HTTP error body before displaying it to the user.
pub(crate) fn redact_response_body(body: &str, truncated: bool) -> String {
    let mut redacted = if let Ok(mut value) = serde_json::from_str::<Value>(body) {
        redact_json(&mut value);
        serde_json::to_string(&value).unwrap_or_else(|_| redact_text_secrets(body))
    } else {
        redact_text_secrets(body)
    };

    if truncated {
        redacted.push_str(HTTP_ERROR_TRUNCATION_MARKER);
    }
    redacted
}

/// Redact credential values from text that resembles a JSON object.
pub(crate) fn redact_text_secrets(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while let Some(relative_quote) = input[index..].find('"') {
        let quote = index + relative_quote;
        output.push_str(&input[index..quote]);

        let Some((key, key_end)) = parse_json_string(input, quote) else {
            output.push_str(&input[quote..]);
            return output;
        };
        output.push_str(&input[quote..key_end]);

        let Some(after_colon) = colon_after_key(input, key_end) else {
            index = key_end;
            continue;
        };

        if !is_secret_key(&key) {
            index = key_end;
            continue;
        }

        output.push_str(&input[key_end..after_colon]);
        let value_start = skip_ascii_whitespace(input, after_colon);
        output.push_str(&input[after_colon..value_start]);

        if input[value_start..].starts_with('"') {
            output.push_str("\"<redacted>\"");
            if let Some((_, value_end)) = parse_json_string(input, value_start) {
                index = value_end;
                continue;
            }
            return output;
        }

        output.push_str("\"<redacted>\"");
        index = next_json_value_boundary(input, value_start);
    }

    output.push_str(&input[index..]);
    output
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if is_secret_key(key) {
                    *child = Value::String("<redacted>".into());
                } else {
                    redact_json(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json(item);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
}

fn parse_json_string(input: &str, quote: usize) -> Option<(String, usize)> {
    if !input[quote..].starts_with('"') {
        return None;
    }

    let mut escaped = false;
    let mut value = String::new();
    for (offset, character) in input[quote + 1..].char_indices() {
        let index = quote + 1 + offset;
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some((value, index + 1));
        } else {
            value.push(character);
        }
    }
    None
}

fn colon_after_key(input: &str, key_end: usize) -> Option<usize> {
    let colon = skip_ascii_whitespace(input, key_end);
    if input[colon..].starts_with(':') {
        Some(colon + 1)
    } else {
        None
    }
}

fn skip_ascii_whitespace(input: &str, mut index: usize) -> usize {
    while let Some(character) = input[index..].chars().next() {
        if !character.is_ascii_whitespace() {
            break;
        }
        index += character.len_utf8();
    }
    index
}

fn next_json_value_boundary(input: &str, start: usize) -> usize {
    input[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            matches!(character, ',' | '}' | ']' | '\n' | '\r').then_some(start + offset)
        })
        .unwrap_or(input.len())
}

#[cfg(test)]
fn auth_headers(auth_token: Option<&str>) -> Option<HeaderMap> {
    let mut headers = HeaderMap::new();
    if let Some(token) = auth_token {
        let value = format!("Bearer {token}").parse().ok()?;
        headers.insert(AUTHORIZATION, value);
    }
    Some(headers)
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
        let headers = auth_headers(Some(&token)).expect("fixture token is a valid header value");
        let authorization = headers
            .get(AUTHORIZATION)
            .expect("authorization header is present")
            .to_str()
            .expect("authorization header is valid text");

        assert_eq!(authorization, format!("Bearer {token}"));
    }

    #[test]
    fn malformed_auth_token_rejected_without_panic() {
        assert!(auth_headers(Some("invalid\ntoken")).is_none());
    }

    #[test]
    fn transport_safety_uses_the_parsed_url_host() {
        assert!(!is_safe_transport(
            "http://127.0.0.1:80@attacker.example/api/config"
        ));
        assert!(is_safe_transport("http://[::1]:7700"));
        assert!(is_safe_transport("http://127.0.0.1:7700"));
        assert!(is_safe_transport("http://127.0.0.2:7700"));
        assert!(is_safe_transport("http://localhost:7700"));
        assert!(!is_safe_transport("http://192.0.2.10:7700"));
        assert!(!is_safe_transport("http://0.0.0.0:7700"));
        assert!(is_safe_transport("https://0.0.0.0:7700"));
        assert!(!is_safe_transport("http://localhost.attacker.example:7700"));
    }
}
