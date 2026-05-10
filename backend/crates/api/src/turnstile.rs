//! Cloudflare Turnstile siteverify wrapper.
//!
//! Turnstile gates new-user creation: the SvelteKit landing page renders
//! the widget, captures a token, and forwards it on the login redirect.
//! `verify` POSTs the secret + token to Cloudflare and returns whether
//! Cloudflare accepted it.
//!
//! When `[turnstile] disabled = true` (default in dev) callers should
//! short-circuit *before* invoking this — the function itself does not
//! consult config, since it has no other reason to know about it.
//!
//! Spec: <https://developers.cloudflare.com/turnstile/get-started/server-side-validation/>

use serde::Deserialize;

const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

#[derive(Debug, Deserialize)]
pub struct VerifyResponse {
    pub success: bool,
    #[serde(default, rename = "error-codes")]
    pub error_codes: Vec<String>,
}

/// Verify a Turnstile token against Cloudflare. Returns the parsed
/// response. Network errors bubble up as `Err`; a `success: false` body
/// is `Ok(...)` so callers can inspect `error_codes` for logging.
pub async fn verify(
    http: &reqwest::Client,
    secret: &str,
    token: &str,
    remote_ip: Option<&str>,
) -> anyhow::Result<VerifyResponse> {
    verify_at(http, SITEVERIFY_URL, secret, token, remote_ip).await
}

/// Same as [`verify`] but with a configurable endpoint, so tests can
/// stand up a local httpmock or wiremock server.
pub async fn verify_at(
    http: &reqwest::Client,
    url: &str,
    secret: &str,
    token: &str,
    remote_ip: Option<&str>,
) -> anyhow::Result<VerifyResponse> {
    let mut form = vec![("secret", secret), ("response", token)];
    if let Some(ip) = remote_ip {
        form.push(("remoteip", ip));
    }
    let resp = http.post(url).form(&form).send().await?;
    let body: VerifyResponse = resp.error_for_status()?.json().await?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_success() {
        let json = r#"{"success":true,"challenge_ts":"2026-01-01","hostname":"example.com"}"#;
        let v: VerifyResponse = serde_json::from_str(json).unwrap();
        assert!(v.success);
        assert!(v.error_codes.is_empty());
    }

    #[test]
    fn parses_failure_with_codes() {
        let json =
            r#"{"success":false,"error-codes":["invalid-input-response","timeout-or-duplicate"]}"#;
        let v: VerifyResponse = serde_json::from_str(json).unwrap();
        assert!(!v.success);
        assert_eq!(v.error_codes.len(), 2);
        assert!(v
            .error_codes
            .contains(&"invalid-input-response".to_string()));
    }

    #[test]
    fn parses_minimal_failure() {
        let json = r#"{"success":false}"#;
        let v: VerifyResponse = serde_json::from_str(json).unwrap();
        assert!(!v.success);
        assert!(v.error_codes.is_empty());
    }
}
