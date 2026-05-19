// ── RUSKY FORK PATCH: _rusky/chat/messages_before ────────────────────────────
//
// Handler for the `_rusky/chat/messages_before` ACP custom method. Backs the
// rusky-app chat infinite-scroll-up loader.
//
// Read path:
//   ACP request → SessionManager::get_session(id, include_messages = true)
//     → Conversation.messages() (chronologically ordered by created_timestamp,
//       insertion id) → cursor index lookup → trailing window of `limit`
//       items immediately preceding the cursor.
//
// Cursor semantics: the response contains messages STRICTLY OLDER than
// `before_message_id`. When the cursor is not present in the session
// the handler returns `{ messages: [], has_more: false }` rather than an
// error — this keeps the FE infinite-scroll fail-soft (treat as "no more
// history" instead of surfacing an unknown-id banner to the user).
//
// `limit` defaults to `DEFAULT_LIMIT` (50) and is hard-capped at
// `MAX_LIMIT` (200) to bound per-call memory.
//
// See SPEC integration TODO removed from
// `rusky-app/src/features/chat/ui/MessageTimeline.tsx` once this method
// is consumed by `useChatHistoryPagination`.

use super::GooseAcpAgent;
use crate::acp::custom_requests::{
    RuskyChatMessagesBeforeRequest, RuskyChatMessagesBeforeResponse,
};
use crate::session::SessionManager;

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;
const METHOD: &str = "_rusky/chat/messages_before";

impl GooseAcpAgent {
    /// Entry point called by `dispatch_custom_request`. Returns
    /// `Some(result)` if the method was handled, `None` to fall through.
    pub async fn dispatch_rusky_chat_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
        if method != METHOD {
            return None;
        }
        let req: RuskyChatMessagesBeforeRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(e) => {
                return Some(Err(
                    agent_client_protocol::Error::invalid_params().data(e.to_string())
                ));
            }
        };
        Some(self.on_chat_messages_before(req).await.and_then(|r| {
            serde_json::to_value(&r).map_err(|e| {
                agent_client_protocol::Error::internal_error().data(e.to_string())
            })
        }))
    }

    /// Real handler — paginates a session's persisted message history.
    pub async fn on_chat_messages_before(
        &self,
        req: RuskyChatMessagesBeforeRequest,
    ) -> Result<RuskyChatMessagesBeforeResponse, agent_client_protocol::Error> {
        chat_messages_before(self.session_manager.as_ref(), req).await
    }
}

/// Pure handler — accepts a `SessionManager` reference so integration
/// tests can drive the read path without constructing a full
/// `GooseAcpAgent` (which requires a provider factory + tauri shell).
pub async fn chat_messages_before(
    session_manager: &SessionManager,
    req: RuskyChatMessagesBeforeRequest,
) -> Result<RuskyChatMessagesBeforeResponse, agent_client_protocol::Error> {
    if req.session_id.trim().is_empty() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("sessionId must not be empty"));
    }
    if req.before_message_id.trim().is_empty() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("beforeMessageId must not be empty"));
    }

    let limit = clamp_limit(req.limit);

    // Read the full conversation from the session store. The store
    // is sqlite-backed and emits messages in chronological order
    // (see `session_manager::get_conversation` — ORDER BY
    // created_timestamp, id).
    let session = session_manager
        .get_session(&req.session_id, /* include_messages */ true)
        .await
        .map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("session read failed: {e}"))
        })?;

    let conversation = match session.conversation {
        Some(c) => c,
        None => {
            // No persisted history for this session — fail-soft.
            tracing::debug!(
                target: "goose::acp::rusky_chat",
                event = "rusky_chat_messages_before",
                outcome = "no_conversation",
                session_id = %req.session_id,
            );
            return Ok(RuskyChatMessagesBeforeResponse::default());
        }
    };

    let messages = conversation.messages();
    let cursor_idx = messages
        .iter()
        .position(|m| m.id.as_deref() == Some(req.before_message_id.as_str()));

    let Some(cursor_idx) = cursor_idx else {
        // Cursor not found: treat as "no more history". The FE
        // halts pagination on `has_more: false` so this is the
        // safest fail-soft behaviour.
        tracing::debug!(
            target: "goose::acp::rusky_chat",
            event = "rusky_chat_messages_before",
            outcome = "cursor_not_found",
            session_id = %req.session_id,
            total = messages.len(),
        );
        return Ok(RuskyChatMessagesBeforeResponse::default());
    };

    if cursor_idx == 0 {
        // Cursor is already the oldest message — nothing older.
        tracing::debug!(
            target: "goose::acp::rusky_chat",
            event = "rusky_chat_messages_before",
            outcome = "at_start",
            session_id = %req.session_id,
        );
        return Ok(RuskyChatMessagesBeforeResponse::default());
    }

    let start = cursor_idx.saturating_sub(limit as usize);
    let slice = &messages[start..cursor_idx];
    let has_more = start > 0;

    let mut out: Vec<serde_json::Value> = Vec::with_capacity(slice.len());
    for m in slice {
        let v = serde_json::to_value(m).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("serialize message: {e}"))
        })?;
        out.push(v);
    }

    tracing::debug!(
        target: "goose::acp::rusky_chat",
        event = "rusky_chat_messages_before",
        outcome = "ok",
        session_id = %req.session_id,
        returned = out.len(),
        has_more = has_more,
        limit = limit,
    );

    Ok(RuskyChatMessagesBeforeResponse {
        messages: out,
        has_more,
    })
}

pub fn clamp_limit(requested: Option<u32>) -> u32 {
    let n = requested.unwrap_or(DEFAULT_LIMIT);
    if n == 0 {
        DEFAULT_LIMIT
    } else if n > MAX_LIMIT {
        MAX_LIMIT
    } else {
        n
    }
}

// ── /RUSKY FORK PATCH: _rusky/chat/messages_before ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_defaults_when_unset() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
    }

    #[test]
    fn clamp_limit_defaults_when_zero() {
        assert_eq!(clamp_limit(Some(0)), DEFAULT_LIMIT);
    }

    #[test]
    fn clamp_limit_caps_at_max() {
        assert_eq!(clamp_limit(Some(1_000)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(MAX_LIMIT)), MAX_LIMIT);
    }

    #[test]
    fn clamp_limit_passes_through_in_range() {
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(199)), 199);
    }
}
