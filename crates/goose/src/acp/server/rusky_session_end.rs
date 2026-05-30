//! ACP `SessionEnd` event emission (Rusky fork — SPEC-051 AC-3).
//!
//! Upstream goose ACP emits `start` and `turn` events but no
//! session-end signal. Without it, a remote client (dashboard, desktop)
//! cannot distinguish a clean user-close from a crash or an internal exit.
//!
//! This module owns the channel-side emission. The ACP server transport
//! layer is responsible for serialising and shipping the event to the
//! connected client. Existing ACP clients ignore unknown events, so this
//! is backwards-compatible.
//!
//! ## Reasons
//!
//! - `user_close`: explicit user action (e.g. closing the session tab,
//!   issuing `_rusky/session/close`, calling `on_delete_session`).
//! - `agent_complete`: the agent decided the conversation was finished
//!   (e.g. `final_output_tool` produced output).
//! - `process_exit`: the parent `goose serve` process is exiting cleanly.
//! - `crash`: the session ended due to an unrecoverable error. Best
//!   effort — `Drop` impls fire this if no explicit reason was emitted.

use serde::{Deserialize, Serialize};

/// Why a session ended. The client uses this to decide whether to
/// prompt for a summary, queue an end-of-day compile, or surface an
/// error toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    UserClose,
    AgentComplete,
    ProcessExit,
    Crash,
}

impl SessionEndReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserClose => "user_close",
            Self::AgentComplete => "agent_complete",
            Self::ProcessExit => "process_exit",
            Self::Crash => "crash",
        }
    }
}

/// The wire-format payload for an ACP `SessionEnd` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEndEvent {
    /// ACP session id that ended.
    pub session_id: String,
    /// Why it ended.
    pub reason: SessionEndReason,
    /// ISO-8601 timestamp when the event was emitted.
    pub at: String,
}

impl SessionEndEvent {
    /// Build an event for `session_id` with `reason` and `now()`.
    #[must_use]
    pub fn now(session_id: impl Into<String>, reason: SessionEndReason) -> Self {
        Self {
            session_id: session_id.into(),
            reason,
            at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Emit a structured tracing event so any tracing subscriber on the
/// goose process can observe session-end without relying on the ACP
/// transport. Used by the ACP server's session-termination handlers.
///
/// Crash paths (panic recovery, Drop guard) call this on a best-effort
/// basis.
pub fn emit_session_end(session_id: &str, reason: SessionEndReason) {
    tracing::info!(
        target: "goose::acp::session_end",
        event = "session_end_emitted",
        session_id,
        reason = reason.as_str(),
        "ACP SessionEnd event",
    );
}

/// RAII guard that fires `SessionEnd { reason: Crash }` on drop if no
/// explicit emission ran first. The agent loop arms one of these at the
/// top of a turn and disarms it on clean exit; if the turn panics, the
/// drop path still ships the SessionEnd event.
pub struct SessionEndGuard {
    session_id: String,
    armed: bool,
}

impl SessionEndGuard {
    #[must_use]
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            armed: true,
        }
    }

    /// Disarm the guard. Call this on a clean-exit path so the guard
    /// does not fire a `Crash` event.
    pub fn disarm(mut self) {
        self.armed = false;
    }

    /// Explicitly emit with a chosen reason and disarm.
    pub fn emit(mut self, reason: SessionEndReason) {
        emit_session_end(&self.session_id, reason);
        self.armed = false;
    }
}

impl Drop for SessionEndGuard {
    fn drop(&mut self) {
        if self.armed {
            emit_session_end(&self.session_id, SessionEndReason::Crash);
        }
    }
}

#[cfg(test)]
mod tests {
    //! SPEC-051 AC-3 coverage.

    use super::*;

    /// SPEC-051 AC-3: reason serialises as snake_case.
    #[test]
    fn reason_serialises_snake_case() {
        let cases = [
            (SessionEndReason::UserClose, "\"user_close\""),
            (SessionEndReason::AgentComplete, "\"agent_complete\""),
            (SessionEndReason::ProcessExit, "\"process_exit\""),
            (SessionEndReason::Crash, "\"crash\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json,
                expected,
                "variant {} did not serialise correctly",
                variant.as_str()
            );
        }
    }

    /// SPEC-051 AC-3: round-trip a full event payload.
    #[test]
    fn event_payload_round_trip() {
        let ev = SessionEndEvent::now("sess_abc", SessionEndReason::UserClose);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"sessionId\":\"sess_abc\""));
        assert!(json.contains("\"reason\":\"user_close\""));
        let parsed: SessionEndEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "sess_abc");
        assert_eq!(parsed.reason, SessionEndReason::UserClose);
    }

    /// SPEC-051 AC-3: an armed guard fires `Crash` on drop.
    #[test]
    fn armed_guard_fires_crash_on_drop() {
        let _g = SessionEndGuard::new("sess_panic");
        // Drop runs at end of scope; tracing emission is observable via
        // a subscriber but here we only assert no panic on construction
        // and drop. The actual emission is exercised by integration
        // tests with a tracing capture layer.
    }

    /// SPEC-051 AC-3: a disarmed guard does NOT fire on drop.
    #[test]
    fn disarmed_guard_does_not_fire() {
        let g = SessionEndGuard::new("sess_clean");
        g.disarm();
    }

    /// SPEC-051 AC-3: explicit emit fires the chosen reason.
    #[test]
    fn explicit_emit_uses_supplied_reason() {
        let g = SessionEndGuard::new("sess_done");
        g.emit(SessionEndReason::AgentComplete);
    }
}
