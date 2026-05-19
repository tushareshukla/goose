//! Integration tests for the `_rusky/chat/messages_before` ACP handler.
//!
//! Exercises the pure `chat_messages_before` entry point against a real
//! sqlite-backed `SessionManager` rooted at a `tempdir`. This is the same
//! storage path the agent uses in production; the only thing skipped is
//! the agent-side dispatch glue, which is just a `serde_json::from_value`
//! wrapper around the call.
//!
//! Scenarios covered:
//!   1. pagination_happy_path     — middle slice returned in order
//!   2. has_more_flag             — last page flips `has_more` to false
//!   3. limit_cap                 — large `limit` is clamped to MAX_LIMIT
//!   4. empty_session             — empty conversation returns []
//!   5. unknown_before_message_id — cursor miss → empty, no error

use goose::acp::server::rusky_chat::{chat_messages_before, MAX_LIMIT};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::session::{SessionManager, SessionType};
use goose_sdk::custom_requests::RuskyChatMessagesBeforeRequest;
use std::path::PathBuf;
use tempfile::TempDir;

async fn fresh_manager() -> (SessionManager, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sm = SessionManager::new(dir.path().to_path_buf());
    (sm, dir)
}

async fn seed_session(sm: &SessionManager, n: usize, prefix: &str) -> String {
    let session = sm
        .create_session(
            PathBuf::from("/tmp"),
            "rusky-chat-test".to_string(),
            SessionType::User,
            GooseMode::default(),
        )
        .await
        .expect("create session");

    for i in 0..n {
        let id = format!("{prefix}-{i:03}");
        let msg = Message::user().with_id(id).with_text(format!("body {i}"));
        sm.add_message(&session.id, &msg)
            .await
            .expect("add message");
        // Insertion order is preserved via sqlite auto-increment id
        // tiebreaker — see `session_manager::get_conversation` ORDER BY.
    }

    session.id
}

#[tokio::test]
async fn pagination_happy_path() {
    let (sm, _dir) = fresh_manager().await;
    let session_id = seed_session(&sm, 10, "p").await;

    // Cursor on the 5th message; ask for 3 older. Expect ids p-001..p-003 (oldest first).
    let resp = chat_messages_before(
        &sm,
        RuskyChatMessagesBeforeRequest {
            session_id: session_id.clone(),
            before_message_id: "p-004".to_string(),
            limit: Some(3),
        },
    )
    .await
    .expect("ok");

    assert_eq!(resp.messages.len(), 3);
    let ids: Vec<String> = resp
        .messages
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["p-001", "p-002", "p-003"]);
    assert!(resp.has_more, "expected p-000 to still be older");
}

#[tokio::test]
async fn has_more_flag_flips_at_session_start() {
    let (sm, _dir) = fresh_manager().await;
    let session_id = seed_session(&sm, 5, "h").await;

    // Cursor on the 3rd message; ask for 5 — slice will start at index 0.
    let resp = chat_messages_before(
        &sm,
        RuskyChatMessagesBeforeRequest {
            session_id,
            before_message_id: "h-002".to_string(),
            limit: Some(5),
        },
    )
    .await
    .expect("ok");

    assert_eq!(resp.messages.len(), 2, "only h-000 and h-001 are older");
    assert!(!resp.has_more, "reached start of session");
}

#[tokio::test]
async fn limit_cap_clamps_to_max() {
    let (sm, _dir) = fresh_manager().await;
    // Seed more than MAX_LIMIT (200) messages so the cap is observable.
    let total = (MAX_LIMIT as usize) + 50;
    let session_id = seed_session(&sm, total, "c").await;
    let cursor = format!("c-{:03}", total - 1);

    let resp = chat_messages_before(
        &sm,
        RuskyChatMessagesBeforeRequest {
            session_id,
            before_message_id: cursor,
            limit: Some(10_000),
        },
    )
    .await
    .expect("ok");

    assert_eq!(resp.messages.len(), MAX_LIMIT as usize);
    assert!(resp.has_more);
}

#[tokio::test]
async fn empty_session_returns_empty_page() {
    let (sm, _dir) = fresh_manager().await;
    let session = sm
        .create_session(
            PathBuf::from("/tmp"),
            "empty".to_string(),
            SessionType::User,
            GooseMode::default(),
        )
        .await
        .expect("create session");

    let resp = chat_messages_before(
        &sm,
        RuskyChatMessagesBeforeRequest {
            session_id: session.id,
            before_message_id: "nonexistent".to_string(),
            limit: None,
        },
    )
    .await
    .expect("ok");

    assert!(resp.messages.is_empty());
    assert!(!resp.has_more);
}

#[tokio::test]
async fn unknown_before_message_id_fails_soft() {
    let (sm, _dir) = fresh_manager().await;
    let session_id = seed_session(&sm, 5, "u").await;

    let resp = chat_messages_before(
        &sm,
        RuskyChatMessagesBeforeRequest {
            session_id,
            before_message_id: "id-not-in-session".to_string(),
            limit: Some(10),
        },
    )
    .await
    .expect("handler must not error on unknown cursor");

    assert!(resp.messages.is_empty());
    assert!(!resp.has_more);
}
