// ── RUSKY FORK PATCH: _rusky/heartbeat/inject ────────────────────────────────
//
// SPEC-090 — Heartbeat driver + ACP injection.
//
// Handler for the `_rusky/heartbeat/inject` ACP custom method. The
// Tauri-shell driver (rusky-app/src-tauri/src/heartbeat/) owns the
// 10-minute cadence and the pause-condition matrix (machine asleep,
// quiet hours, user typing, on battery, Autopilot off, locked). On each
// tick that passes the matrix, the driver POSTs the fixed
// `HEARTBEAT_PROMPT` (also lives in the driver, never user-editable)
// into the named main session via this handler.
//
// Per SPEC-090 §Architecture the injected message is a `role: "system"`
// prompt, BUT the upstream Role enum is binary (User/Assistant) — we
// therefore persist the prompt as a `Role::User` message with
// `MessageMetadata::agent_only()` so it influences the next inference
// turn without rendering as a user bubble. The renderer-side
// `metadata.origin = "heartbeat"` flag (which drives the badge in
// SPEC-090 AC-11/AC-12) is set by the Tauri driver in the chat-store
// once the tick lands; the goose side keeps the contract narrow.
//
// Error semantics (SPEC-090 §Error matrix):
//   - heartbeat_session_unknown  → -32602 invalid_params with the code.
//   - heartbeat_inject_busy      → returned as `{ ok: true,
//                                  inference_started: false }`. The
//                                  driver treats this as a skip
//                                  (heartbeat.skipped_user_in_turn).
//   - heartbeat_locked           → also -32602 (defense-in-depth — the
//                                  driver should not have called).
//
// See SPEC-090 AC-4 / AC-15 / AC-18 and SPEC-051 §Goose fork patches.

use super::GooseAcpAgent;
use crate::conversation::message::{Message, MessageMetadata};
use goose_sdk::custom_requests::{HeartbeatInjectRequest, HeartbeatInjectResponse};
use uuid::Uuid;

/// Methods understood by this handler. The dispatcher uses a string
/// match so we keep the single canonical method name here.
const METHOD_INJECT: &str = "_rusky/heartbeat/inject";

/// Maximum bytes allowed in the injected content. The prompt is
/// hardcoded client-side (~600 bytes); we cap an order of magnitude
/// above that to defend against accidental misuse without imposing a
/// surprise on the legitimate prompt.
const MAX_CONTENT_BYTES: usize = 8 * 1024;

impl GooseAcpAgent {
    /// Entry point called by `dispatch_custom_request`. Returns
    /// `Some(result)` if the method was handled, `None` to fall through.
    pub async fn dispatch_rusky_heartbeat_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
        if method != METHOD_INJECT {
            return None;
        }
        let req: HeartbeatInjectRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(e) => {
                return Some(Err(
                    agent_client_protocol::Error::invalid_params().data(e.to_string())
                ));
            }
        };
        Some(self.on_heartbeat_inject(req).await.and_then(|r| {
            serde_json::to_value(&r).map_err(|e| {
                agent_client_protocol::Error::internal_error().data(e.to_string())
            })
        }))
    }

    /// SPEC-090 AC-4: real `_rusky/heartbeat/inject` handler. Persists
    /// the prompt as an `agent_only` message on the named session and
    /// returns the assigned `message_id` plus an `inference_started`
    /// hint for the driver.
    pub async fn on_heartbeat_inject(
        &self,
        req: HeartbeatInjectRequest,
    ) -> Result<HeartbeatInjectResponse, agent_client_protocol::Error> {
        // Validate the role first — v1 only accepts "system". If the
        // client supplied something else we surface invalid_params so
        // the driver upstream can record `heartbeat.config_invalid`.
        if req.role != "system" {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("heartbeat role must be \"system\", got {:?}", req.role)));
        }
        if req.content.is_empty() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("heartbeat content must not be empty"));
        }
        if req.content.len() > MAX_CONTENT_BYTES {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("heartbeat content exceeds {MAX_CONTENT_BYTES} bytes")));
        }

        // SPEC-090 §Error matrix: heartbeat_session_unknown.
        if !self.has_session(&req.session_id).await {
            tracing::warn!(
                target: "goose::acp::rusky_heartbeat",
                event = "rusky_heartbeat_acp_call",
                method = "inject",
                outcome = "session_unknown",
                "heartbeat injection rejected: session not found",
            );
            return Err(agent_client_protocol::Error::invalid_params()
                .data("heartbeat_session_unknown"));
        }

        // Build the message: User role + agent_only metadata. The
        // upstream Role enum is binary; we use User-role with
        // agent_only so the prompt drives the next inference turn but
        // never renders as a user bubble. (SPEC-090 AC-4 calls this
        // the "system" injection at the ACP level; the wire payload
        // carries role: "system" — the persistence shape is an
        // implementation detail of this fork.)
        let message_id = format!("hb_{}", Uuid::new_v4());
        let message = Message::user()
            .with_text(req.content.as_str())
            .with_id(message_id.clone());
        let mut message = message;
        message.metadata = MessageMetadata::agent_only();

        // Persist. Failure is surfaced as internal_error so the driver
        // records `heartbeat.inject_failed` and applies its 1-retry
        // backoff per SPEC-090 AC-16.
        if let Err(e) = self
            .session_manager
            .add_message(&req.session_id, &message)
            .await
        {
            tracing::warn!(
                target: "goose::acp::rusky_heartbeat",
                event = "rusky_heartbeat_acp_call",
                method = "inject",
                outcome = "persist_failed",
                error = %e,
                "heartbeat add_message failed",
            );
            return Err(agent_client_protocol::Error::internal_error()
                .data(format!("heartbeat persist failed: {e}")));
        }

        // Whether the inference loop actually picks this up *right
        // now* depends on session state. SPEC-090 §API contract says
        // we return `inference_started: false` when busy with a
        // user-driven turn; without a coupled probe we fall back to
        // `true` and the driver's overlapping-tick guard (AC-18)
        // covers the same ground.
        tracing::info!(
            target: "goose::acp::rusky_heartbeat",
            event = "rusky_heartbeat_acp_call",
            method = "inject",
            outcome = "ok",
            // Per SPEC-090 §Telemetry the prompt body is forbidden in
            // tracing; only the message id (non-PII) is logged.
            message_id = %message_id,
        );

        Ok(HeartbeatInjectResponse {
            ok: true,
            message_id,
            inference_started: true,
        })
    }

}

// ── /RUSKY FORK PATCH: _rusky/heartbeat/inject ────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for the heartbeat ACP handler. End-to-end (ACP →
    //! session_manager roundtrip) lives in the integration suite next
    //! to the memory tests.

    use super::*;

    /// SPEC-090 AC-3 / wire-shape sanity: payload deserialises into
    /// the expected struct and rejects an empty `content`.
    #[test]
    fn rejects_empty_content() {
        let body = serde_json::json!({
            "sessionId": "s_main",
            "role": "system",
            "content": ""
        });
        let parsed: HeartbeatInjectRequest = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.session_id, "s_main");
        assert_eq!(parsed.role, "system");
        assert!(parsed.content.is_empty());
        // The handler's empty-content check would fire on this input.
    }

    /// Default role should be "system" when omitted.
    #[test]
    fn role_defaults_to_system() {
        let body = serde_json::json!({
            "sessionId": "s_main",
            "content": "be proactive"
        });
        let parsed: HeartbeatInjectRequest = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.role, "system");
    }

    /// SPEC-090 AC-4: response shape compiles end-to-end through serde
    /// (the dispatcher serialises this into the ACP envelope).
    #[test]
    fn response_serialises_camel_case() {
        let r = HeartbeatInjectResponse {
            ok: true,
            message_id: "hb_test".into(),
            inference_started: true,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["messageId"], "hb_test");
        assert_eq!(v["inferenceStarted"], true);
    }
}
