//! Integration tests for the `_rusky/network/test_connection` ACP handler.
//!
//! Spins up a wiremock server impersonating the rusky-proxy `/health`
//! endpoint and drives the request through the real ACP dispatch path
//! (`send_custom`). Confirms:
//!
//!   1. happy path  — 200 `/health` → `{ ok: true, latencyMs: > 0 }`
//!   2. 5xx response → `{ ok: false, error: "proxy returned HTTP 503" }`
//!   3. unreachable URL (connection refused) → `{ ok: false, error: ... }`
//!   4. malformed override URL → `{ ok: false, error: "invalid proxy URL" }`

#[allow(dead_code)]
#[path = "acp_common_tests/mod.rs"]
mod common_tests;

use common_tests::fixtures::server::AcpServerConnection;
use common_tests::fixtures::{
    run_test, send_custom, Connection, OpenAiFixture, TestConnectionConfig,
};
use goose_test_support::EnforceSessionId;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn network_test_connection_returns_ok_on_2xx() {
    run_test(async {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status":"ok"}"#))
            .mount(&mock)
            .await;

        let openai = OpenAiFixture::new(vec![], Arc::new(EnforceSessionId::default())).await;
        let conn = AcpServerConnection::new(TestConnectionConfig::default(), openai).await;

        let response = send_custom(
            conn.cx(),
            "_rusky/network/test_connection",
            serde_json::json!({ "proxyUrl": mock.uri() }),
        )
        .await
        .expect("test_connection should succeed");

        assert_eq!(response.get("ok"), Some(&serde_json::json!(true)));
        let latency = response
            .get("latencyMs")
            .and_then(|v| v.as_u64())
            .expect("latencyMs should be a number on success");
        // Sanity: 5s timeout cap, so a single round trip is well under.
        assert!(latency < 5_000, "latency {latency}ms within timeout");
        assert!(response.get("error").is_none());
    });
}

#[test]
fn network_test_connection_returns_error_on_5xx() {
    run_test(async {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock)
            .await;

        let openai = OpenAiFixture::new(vec![], Arc::new(EnforceSessionId::default())).await;
        let conn = AcpServerConnection::new(TestConnectionConfig::default(), openai).await;

        let response = send_custom(
            conn.cx(),
            "_rusky/network/test_connection",
            serde_json::json!({ "proxyUrl": mock.uri() }),
        )
        .await
        .expect("test_connection should succeed at the ACP level");

        assert_eq!(response.get("ok"), Some(&serde_json::json!(false)));
        assert!(response.get("latencyMs").is_none());
        let err = response
            .get("error")
            .and_then(|v| v.as_str())
            .expect("error message present on failure");
        assert!(err.contains("503"), "error mentions status: {err}");
    });
}

#[test]
fn network_test_connection_returns_error_on_unreachable() {
    run_test(async {
        let openai = OpenAiFixture::new(vec![], Arc::new(EnforceSessionId::default())).await;
        let conn = AcpServerConnection::new(TestConnectionConfig::default(), openai).await;

        // 127.0.0.1:1 is reserved and refuses connections. The probe
        // surfaces a `connection error: ...` body, never an ACP error.
        let response = send_custom(
            conn.cx(),
            "_rusky/network/test_connection",
            serde_json::json!({ "proxyUrl": "http://127.0.0.1:1" }),
        )
        .await
        .expect("test_connection should resolve at the ACP level");

        assert_eq!(response.get("ok"), Some(&serde_json::json!(false)));
        assert!(response
            .get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("connection error")));
    });
}

#[test]
fn network_test_connection_rejects_malformed_override_url() {
    run_test(async {
        let openai = OpenAiFixture::new(vec![], Arc::new(EnforceSessionId::default())).await;
        let conn = AcpServerConnection::new(TestConnectionConfig::default(), openai).await;

        // Non-URL string. The resolver fails fast and surfaces the
        // error string without issuing an HTTP call.
        let response = send_custom(
            conn.cx(),
            "_rusky/network/test_connection",
            serde_json::json!({ "proxyUrl": "not a url" }),
        )
        .await
        .expect("test_connection should resolve at the ACP level");

        assert_eq!(response.get("ok"), Some(&serde_json::json!(false)));
        let err = response
            .get("error")
            .and_then(|v| v.as_str())
            .expect("error message present");
        assert!(
            err.to_lowercase().contains("invalid proxy url"),
            "error mentions invalid URL: {err}"
        );
    });
}
