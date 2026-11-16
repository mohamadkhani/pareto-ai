//! Credential health probe: `GET {base_url}/models` with the credential's
//! merged headers and its secret as bearer auth (the OpenAI-compatible
//! discovery endpoint, per the M2 spec).
//!
//! The secret goes on the wire to the provider and nowhere else: it is never
//! logged, and [`ProbeResult`] carries only status, latency, and a
//! transport-level error summary. Custom headers come pre-merged from
//! [`crate::catalog::headers::merge_headers`] (credential headers, overridden
//! per-offering when M3 asks for it).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Timeout for the probe round trip. Interactive endpoint: tighter than the
/// 15s catalog sync budget.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The outcome of one probe, safe to log and to store on the credential.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// True only for an HTTP 2xx response.
    pub ok: bool,
    /// Upstream HTTP status, when a response arrived.
    pub http_status: Option<i64>,
    pub latency_ms: u64,
    /// Transport-level failure summary; never contains header or secret
    /// values.
    pub error: Option<String>,
}

impl ProbeResult {
    /// The status string recorded on the credential row: `ok` or `error`.
    pub fn status(&self) -> &'static str {
        match self.ok {
            true => "ok",
            false => "error",
        }
    }
}

/// Probes `GET {base_url}/models` with `headers` plus `Authorization: Bearer
/// <secret>`, unless a header already overrides `authorization`.
///
/// Never fails: transport errors and bad header values become a failed
/// [`ProbeResult`], so a broken credential still gets its `status` and
/// `last_tested_at` recorded.
pub async fn probe_models(
    base_url: &str,
    headers: &BTreeMap<String, String>,
    secret: Option<&str>,
) -> ProbeResult {
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => return failure(None, 0, e.to_string()),
    };

    let mut request = client.get(&url);
    if let Some(secret) = secret
        && !headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
    {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {secret}"));
    }
    for (name, value) in headers {
        match reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
            Ok(name) => match reqwest::header::HeaderValue::from_str(value) {
                Ok(value) => {
                    request = request.header(name, value);
                }
                Err(_) => return failure(None, 0, format!("header {name} has an invalid value")),
            },
            Err(_) => return failure(None, 0, format!("header {name} is not a valid name")),
        }
    }

    let started = Instant::now();
    let response = request.send().await;
    let latency_ms = started.elapsed().as_millis() as u64;

    match response {
        Ok(response) => {
            let status = response.status();
            ProbeResult {
                ok: status.is_success(),
                http_status: Some(i64::from(status.as_u16())),
                latency_ms,
                error: None,
            }
        }
        Err(e) => failure(None, latency_ms, e.to_string()),
    }
}

fn failure(http_status: Option<i64>, latency_ms: u64, error: String) -> ProbeResult {
    ProbeResult {
        ok: false,
        http_status,
        latency_ms,
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_static_server;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn probe_sends_the_secret_and_headers_and_reads_the_status() {
        let (addr, seen) = spawn_static_server("200 OK", r#"{"data":[]}"#).await;
        let base_url = format!("http://{addr}/v1");
        let header_map = headers(&[("x-tenant", "team-a")]);

        let result = probe_models(&base_url, &header_map, Some("sk-live-secret")).await;
        assert!(result.ok);
        assert_eq!(result.http_status, Some(200));
        assert_eq!(result.status(), "ok");
        assert!(result.error.is_none());

        let captured = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(captured.len(), 1);
        let request = &captured[0];
        assert!(
            request.starts_with("GET /v1/models HTTP/1.1"),
            "probe hits {{base_url}}/models: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-live-secret"),
            "the secret authenticates the probe: {request}"
        );
        assert!(
            request.to_ascii_lowercase().contains("x-tenant: team-a"),
            "custom headers travel with the probe: {request}"
        );
    }

    #[tokio::test]
    async fn an_explicit_authorization_header_overrides_the_default_bearer() {
        let (addr, seen) = spawn_static_server("200 OK", "{}").await;
        let base_url = format!("http://{addr}");
        let header_map = headers(&[("Authorization", "Api-Key xyz")]);

        let result = probe_models(&base_url, &header_map, Some("sk-live-secret")).await;
        assert!(result.ok);

        let captured = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let request = captured[0].to_ascii_lowercase();
        assert!(request.contains("authorization: api-key xyz"), "{request}");
        assert!(
            !request.contains("bearer sk-live-secret"),
            "the default bearer must not be sent when overridden"
        );
    }

    #[tokio::test]
    async fn non_2xx_is_recorded_as_a_failed_probe() {
        let (addr, _) = spawn_static_server("401 Unauthorized", r#"{"error":"nope"}"#).await;
        let base_url = format!("http://{addr}");

        let result = probe_models(&base_url, &BTreeMap::new(), Some("sk")).await;
        assert!(!result.ok);
        assert_eq!(result.http_status, Some(401));
        assert_eq!(result.status(), "error");
    }

    #[tokio::test]
    async fn transport_errors_never_panic_and_stay_secret_free() {
        // Nothing listens on port 1: the connection is refused.
        let result = probe_models(
            "http://127.0.0.1:1",
            &headers(&[("x-secret-header", "do-not-leak")]),
            Some("sk-live-secret"),
        )
        .await;
        assert!(!result.ok);
        assert_eq!(result.status(), "error");
        assert!(result.http_status.is_none());
        let error = result.error.expect("an error summary");
        assert!(!error.contains("sk-live-secret"), "{error}");
        assert!(!error.contains("do-not-leak"), "{error}");
    }

    #[test]
    fn probe_result_status_mapping_is_stable() {
        let ok = ProbeResult {
            ok: true,
            http_status: Some(204),
            latency_ms: 1,
            error: None,
        };
        let failed = ProbeResult {
            ok: false,
            http_status: None,
            latency_ms: 1,
            error: Some("boom".to_string()),
        };
        assert_eq!(ok.status(), "ok");
        assert_eq!(failed.status(), "error");
    }
}
