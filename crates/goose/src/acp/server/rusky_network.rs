// ── RUSKY FORK PATCH: _rusky/network/test_connection ─────────────────────────
//
// Connectivity probe for the rusky-proxy. The Network settings pane
// surfaces this as a "Test connection" button; the handler does a
// short-timeout HTTP GET against the proxy's `/health` endpoint using
// either an override URL (from the request) or the configured
// `networkProxyUrl` preference. Falls back to `RUSKY_PROXY_URL` and
// finally to `http://127.0.0.1:8080` so a freshly-installed app probes
// the local dev proxy by default.
//
// Wire shape lives in `goose-sdk` (`NetworkTestConnectionRequest` /
// `NetworkTestConnectionResponse`). Response error strings are safe to
// surface in the settings pane — no provider tokens or PII.

use super::GooseAcpAgent;
use crate::config::Config;
use goose_sdk::custom_requests::{NetworkTestConnectionRequest, NetworkTestConnectionResponse};
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CONFIG_KEY_PROXY_URL: &str = "RUSKY_NETWORK_PROXY_URL";
const PROXY_URL_ENV: &str = "RUSKY_PROXY_URL";
const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:8080";

impl GooseAcpAgent {
    /// Resolve the proxy base URL, then issue a short-timeout
    /// `GET <base>/health`. Returns a structured response — the handler
    /// never raises ACP-level errors for "couldn't reach the proxy";
    /// that goes in the response body so the pane can render it.
    pub(super) async fn on_network_test_connection(
        &self,
        req: NetworkTestConnectionRequest,
    ) -> Result<NetworkTestConnectionResponse, agent_client_protocol::Error> {
        let base = self.resolve_proxy_base_url(req.proxy_url.as_deref());
        let base = match base {
            Ok(b) => b,
            Err(msg) => {
                tracing::warn!(
                    target: "goose::acp::rusky_network",
                    event = "rusky_network_test_connection",
                    outcome = "resolve_failed",
                    error = %msg,
                );
                return Ok(NetworkTestConnectionResponse {
                    ok: false,
                    latency_ms: None,
                    error: Some(msg),
                });
            }
        };
        let url = format!("{}/health", base.trim_end_matches('/'));

        let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(NetworkTestConnectionResponse {
                    ok: false,
                    latency_ms: None,
                    error: Some(format!("failed to build HTTP client: {e}")),
                });
            }
        };

        let start = Instant::now();
        let result = client.get(&url).send().await;
        let elapsed_ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;

        let response = match result {
            Ok(resp) if resp.status().is_success() => NetworkTestConnectionResponse {
                ok: true,
                latency_ms: Some(elapsed_ms),
                error: None,
            },
            Ok(resp) => NetworkTestConnectionResponse {
                ok: false,
                latency_ms: None,
                error: Some(format!("proxy returned HTTP {}", resp.status().as_u16())),
            },
            Err(e) => NetworkTestConnectionResponse {
                ok: false,
                latency_ms: None,
                error: Some(format!("connection error: {}", scrub_error(&e))),
            },
        };

        tracing::info!(
            target: "goose::acp::rusky_network",
            event = "rusky_network_test_connection",
            outcome = if response.ok { "ok" } else { "fail" },
            latency_ms = response.latency_ms.unwrap_or(0),
        );

        Ok(response)
    }

    /// Resolution chain:
    ///   1. explicit request override (`req.proxy_url`)
    ///   2. configured `RUSKY_NETWORK_PROXY_URL` preference
    ///   3. `RUSKY_PROXY_URL` environment variable
    ///   4. built-in default (`http://127.0.0.1:8080`)
    fn resolve_proxy_base_url(&self, override_url: Option<&str>) -> Result<String, String> {
        if let Some(raw) = override_url.map(str::trim).filter(|s| !s.is_empty()) {
            return validate_url(raw);
        }
        let config = self
            .config()
            .map_err(|e| format!("config unavailable: {e:?}"))?;
        if let Some(stored) = optional_config_string(&config, CONFIG_KEY_PROXY_URL) {
            if !stored.trim().is_empty() {
                return validate_url(stored.trim());
            }
        }
        if let Ok(env) = std::env::var(PROXY_URL_ENV) {
            let env = env.trim().to_string();
            if !env.is_empty() {
                return validate_url(&env);
            }
        }
        Ok(DEFAULT_PROXY_URL.to_string())
    }
}

fn validate_url(raw: &str) -> Result<String, String> {
    url::Url::parse(raw)
        .map(|_| raw.to_string())
        .map_err(|e| format!("invalid proxy URL: {e}"))
}

fn optional_config_string(config: &Config, key: &str) -> Option<String> {
    config.get_param::<String>(key).ok()
}

/// Trim the request URL out of reqwest error display strings — they
/// often echo back the full URL. The base URL is already visible in
/// the pane; we don't need it inside the error label too.
fn scrub_error(err: &reqwest::Error) -> String {
    let msg = err.to_string();
    if let Some((_, tail)) = msg.rsplit_once(": ") {
        if !tail.is_empty() {
            return tail.to_string();
        }
    }
    msg
}

// ── /RUSKY FORK PATCH: _rusky/network/test_connection ────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserialises_camel_case() {
        let body = serde_json::json!({ "proxyUrl": "http://example.com:1234" });
        let parsed: NetworkTestConnectionRequest = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.proxy_url.as_deref(), Some("http://example.com:1234"));
    }

    #[test]
    fn request_allows_empty_body() {
        let body = serde_json::json!({});
        let parsed: NetworkTestConnectionRequest = serde_json::from_value(body).unwrap();
        assert!(parsed.proxy_url.is_none());
    }

    #[test]
    fn response_serialises_camel_case() {
        let r = NetworkTestConnectionResponse {
            ok: true,
            latency_ms: Some(42),
            error: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["latencyMs"], 42);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn validate_url_rejects_non_url() {
        assert!(validate_url("not a url").is_err());
        assert!(validate_url("").is_err());
        assert!(validate_url("http://valid.example").is_ok());
    }

    #[test]
    fn default_base_url_is_local_proxy() {
        assert_eq!(DEFAULT_PROXY_URL, "http://127.0.0.1:8080");
    }
}
