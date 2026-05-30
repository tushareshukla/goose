// ── RUSKY FORK PATCH: _rusky/memory/* ────────────────────────────────────────
//
// Real ACP handlers for the `_rusky/memory/*` method family.
//
// These handlers proxy each ACP method to the corresponding HTTP route on
// `${RUSKY_PROXY_URL}/v1/memory/*`. The proxy's routes (in
// `rusky-proxy/src/memory/routes.rs`) call into the existing memory
// workers (`worker_memory.rs` / `store.rs` / `ranker.rs`).
//
// Auth model (Option A — see SPEC-051 commit message):
//
//   The ACP server inside `goose serve` does NOT carry a session JWT.
//   Both processes share a local-only token via the
//   `RUSKY_LOCAL_AGENT_TOKEN` env var, set at install time. The token
//   is sent in the `X-Rusky-Local-Agent` header. The proxy's local-agent
//   middleware (on `/v1/memory/*` only) accepts this token instead of
//   a JWT. Local-only — does not help an attacker outside the install.
//
// See SPEC-051 §AC-4, §AC-5, §AC-6 and SPEC-040.

use super::GooseAcpAgent;
use crate::acp::custom_requests::{
    MemoryCompileRequest, MemoryCompileResponse, MemoryFlushRequest, MemoryFlushResponse,
    MemoryForgetRequest, MemoryForgetResponse, MemoryHit, MemorySearchRequest,
    MemorySearchResponse, MemoryStatusRequest, MemoryStatusResponse,
};

const PROXY_URL_ENV: &str = "RUSKY_PROXY_URL";
const LOCAL_AGENT_ENV: &str = "RUSKY_LOCAL_AGENT_TOKEN";
const LOCAL_AGENT_HEADER: &str = "X-Rusky-Local-Agent";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Best-effort proxy URL discovery. Falls back to localhost:8080
/// (the proxy's default dev port) when env is unset.
fn proxy_url() -> String {
    std::env::var(PROXY_URL_ENV).unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn local_token() -> Option<String> {
    std::env::var(LOCAL_AGENT_ENV)
        .ok()
        .filter(|s| !s.is_empty())
}

/// Build the reqwest client used for ACP→proxy memory calls.
/// Reqwest is already a goose dependency.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
}

/// Internal helper: POST a JSON body to `/v1/memory/<route>` and return
/// the parsed response on success. Returns a JSON-RPC compatible error
/// on any failure.
async fn post_json<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
    route: &str,
    body: &Req,
) -> Result<Resp, agent_client_protocol::Error> {
    let Some(token) = local_token() else {
        // No token configured = feature disabled. Surface a -32603
        // internal error so the client can render an actionable message.
        tracing::warn!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = route,
            outcome = "no_local_token",
            "RUSKY_LOCAL_AGENT_TOKEN not set; memory ACP call rejected",
        );
        return Err(agent_client_protocol::Error::internal_error()
            .data("local agent token not configured; restart the install"));
    };

    let url = format!("{}/v1/memory/{}", proxy_url().trim_end_matches('/'), route);
    let client = http_client();
    let resp = client
        .post(&url)
        .header(LOCAL_AGENT_HEADER, token)
        .json(body)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "goose::acp::rusky_memory",
                event = "rusky_memory_acp_call",
                method = route,
                outcome = "network_error",
                error = %e,
                "memory proxy unreachable",
            );
            agent_client_protocol::Error::internal_error().data(format!("proxy unreachable: {e}"))
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = route,
            outcome = "http_error",
            status = status.as_u16(),
            "memory proxy returned non-2xx",
        );
        return Err(agent_client_protocol::Error::internal_error()
            .data(format!("proxy returned {status}: {body}")));
    }

    resp.json::<Resp>().await.map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to parse proxy response: {e}"))
    })
}

/// Internal helper: GET `/v1/memory/<route>` and parse the JSON response.
async fn get_json<Resp: serde::de::DeserializeOwned>(
    route: &str,
) -> Result<Resp, agent_client_protocol::Error> {
    let Some(token) = local_token() else {
        return Err(agent_client_protocol::Error::internal_error()
            .data("local agent token not configured; restart the install"));
    };
    let url = format!("{}/v1/memory/{}", proxy_url().trim_end_matches('/'), route);
    let client = http_client();
    let resp = client
        .get(&url)
        .header(LOCAL_AGENT_HEADER, token)
        .send()
        .await
        .map_err(|e| {
            agent_client_protocol::Error::internal_error().data(format!("proxy unreachable: {e}"))
        })?;
    if !resp.status().is_success() {
        return Err(agent_client_protocol::Error::internal_error()
            .data(format!("proxy returned {}", resp.status())));
    }
    resp.json::<Resp>().await.map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to parse proxy response: {e}"))
    })
}

// ─── Proxy DTOs (kept private to this module) ───────────────────────────────
//
// We avoid sharing types between goose and rusky-proxy crates (would
// require a new shared crate). The DTOs below mirror the proxy's
// route bodies; the proxy owns the schema, this module owns the
// client-side view.

#[derive(serde::Serialize)]
struct FlushBody<'a> {
    session_id: Option<&'a str>,
    flush_text: &'a str,
    last_message_id: u64,
    user_id: Option<&'a str>,
}

#[derive(Debug, serde::Deserialize)]
struct FlushReply {
    ok: bool,
    turns_flushed: u32,
    log_path: Option<String>,
}

#[derive(serde::Serialize)]
struct CompileBody<'a> {
    date: Option<&'a str>,
    force: bool,
    user_id: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct CompileReply {
    ok: bool,
    entries_written: u32,
    date: String,
    skipped: bool,
}

#[derive(serde::Serialize)]
struct SearchBody<'a> {
    query: &'a str,
    top_k: Option<u32>,
    scopes: Option<&'a Vec<String>>,
    user_id: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct SearchReply {
    hits: Vec<SearchHit>,
    total_searched: u32,
}

#[derive(serde::Deserialize)]
struct SearchHit {
    id: String,
    score: f32,
    date: String,
    summary: String,
}

#[derive(serde::Serialize)]
struct ForgetBody<'a> {
    path: Option<&'a str>,
    query: Option<&'a str>,
    user_id: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct ForgetReply {
    ok: bool,
    deleted: bool,
}

#[derive(serde::Deserialize)]
struct StatusReply {
    ok: bool,
    status: String,
    total_entries: u32,
    last_compiled_date: Option<String>,
    store_size_bytes: u64,
    worker_queue_depth: u32,
}

// ─── ACP dispatch ────────────────────────────────────────────────────────────

impl GooseAcpAgent {
    /// Entry point called by `dispatch_custom_request` when the
    /// `rusky-memory` feature is enabled. Returns `Some(result)` if the
    /// method was handled, `None` to fall through to other dispatchers.
    pub async fn dispatch_rusky_memory_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
        match method {
            "_rusky/memory/flush" => {
                let req: MemoryFlushRequest = match serde_json::from_value(params) {
                    Ok(r) => r,
                    Err(e) => {
                        return Some(Err(
                            agent_client_protocol::Error::invalid_params().data(e.to_string())
                        ))
                    }
                };
                Some(self.on_memory_flush(req).await.and_then(|r| {
                    serde_json::to_value(&r).map_err(|e| {
                        agent_client_protocol::Error::internal_error().data(e.to_string())
                    })
                }))
            }
            "_rusky/memory/compile" => {
                let req: MemoryCompileRequest = match serde_json::from_value(params) {
                    Ok(r) => r,
                    Err(e) => {
                        return Some(Err(
                            agent_client_protocol::Error::invalid_params().data(e.to_string())
                        ))
                    }
                };
                Some(self.on_memory_compile(req).await.and_then(|r| {
                    serde_json::to_value(&r).map_err(|e| {
                        agent_client_protocol::Error::internal_error().data(e.to_string())
                    })
                }))
            }
            "_rusky/memory/search" => {
                let req: MemorySearchRequest = match serde_json::from_value(params) {
                    Ok(r) => r,
                    Err(e) => {
                        return Some(Err(
                            agent_client_protocol::Error::invalid_params().data(e.to_string())
                        ))
                    }
                };
                Some(self.on_memory_search(req).await.and_then(|r| {
                    serde_json::to_value(&r).map_err(|e| {
                        agent_client_protocol::Error::internal_error().data(e.to_string())
                    })
                }))
            }
            "_rusky/memory/forget" => {
                let req: MemoryForgetRequest = match serde_json::from_value(params) {
                    Ok(r) => r,
                    Err(e) => {
                        return Some(Err(
                            agent_client_protocol::Error::invalid_params().data(e.to_string())
                        ))
                    }
                };
                Some(self.on_memory_forget(req).await.and_then(|r| {
                    serde_json::to_value(&r).map_err(|e| {
                        agent_client_protocol::Error::internal_error().data(e.to_string())
                    })
                }))
            }
            "_rusky/memory/status" => {
                let req: MemoryStatusRequest = match serde_json::from_value(params) {
                    Ok(r) => r,
                    Err(e) => {
                        return Some(Err(
                            agent_client_protocol::Error::invalid_params().data(e.to_string())
                        ))
                    }
                };
                Some(self.on_memory_status(req).await.and_then(|r| {
                    serde_json::to_value(&r).map_err(|e| {
                        agent_client_protocol::Error::internal_error().data(e.to_string())
                    })
                }))
            }
            _ => None,
        }
    }

    /// SPEC-051 AC-2: real `_rusky/memory/flush` handler.
    pub async fn on_memory_flush(
        &self,
        req: MemoryFlushRequest,
    ) -> Result<MemoryFlushResponse, agent_client_protocol::Error> {
        // NOTE: `EnteredSpan` is not Send; ACP dispatch crosses await
        // points on a single-threaded runtime that requires Send. We emit
        // the structured log events without entering a span.
        // SPEC-040 daily-log block needs at least a session marker. When
        // the caller didn't supply turns to extract we POST an empty
        // string and the proxy reports FLUSH_OK with 0 turns.
        let body = FlushBody {
            session_id: req.session_id.as_deref(),
            flush_text: "",
            last_message_id: 0,
            user_id: None,
        };
        let reply: FlushReply = post_json("flush", &body).await?;
        tracing::info!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = "flush",
            outcome = if reply.ok { "ok" } else { "error" },
            turns_flushed = reply.turns_flushed,
        );
        Ok(MemoryFlushResponse {
            turns_flushed: reply.turns_flushed,
            log_path: reply.log_path,
        })
    }

    /// SPEC-051 AC-2: real `_rusky/memory/compile` handler.
    pub async fn on_memory_compile(
        &self,
        req: MemoryCompileRequest,
    ) -> Result<MemoryCompileResponse, agent_client_protocol::Error> {
        // See on_memory_flush comment re: Send constraint.
        let body = CompileBody {
            date: req.date.as_deref(),
            force: req.force,
            user_id: None,
        };
        let reply: CompileReply = post_json("compile", &body).await?;
        tracing::info!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = "compile",
            outcome = if reply.ok { "ok" } else { "error" },
            entries_written = reply.entries_written,
            skipped = reply.skipped,
        );
        Ok(MemoryCompileResponse {
            entries_written: reply.entries_written,
            date: reply.date,
            skipped: reply.skipped,
        })
    }

    /// SPEC-051 AC-2: real `_rusky/memory/search` handler.
    pub async fn on_memory_search(
        &self,
        req: MemorySearchRequest,
    ) -> Result<MemorySearchResponse, agent_client_protocol::Error> {
        // See on_memory_flush comment re: Send constraint.
        if req.query.trim().is_empty() {
            return Err(
                agent_client_protocol::Error::invalid_params().data("query must not be empty")
            );
        }
        let body = SearchBody {
            query: &req.query,
            top_k: req.limit,
            scopes: None,
            user_id: None,
        };
        let reply: SearchReply = post_json("search", &body).await?;
        // NOTE: query string and hit summaries MUST NEVER appear in
        // tracing spans (SPEC-051 §Telemetry). Counts only.
        tracing::info!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = "search",
            outcome = "ok",
            hits_returned = reply.hits.len() as u32,
            total_searched = reply.total_searched,
        );
        Ok(MemorySearchResponse {
            hits: reply
                .hits
                .into_iter()
                .map(|h| MemoryHit {
                    id: h.id,
                    score: h.score,
                    date: h.date,
                    summary: h.summary,
                    content: None,
                })
                .collect(),
            total_searched: reply.total_searched,
        })
    }

    /// SPEC-051 AC-2: real `_rusky/memory/forget` handler.
    pub async fn on_memory_forget(
        &self,
        req: MemoryForgetRequest,
    ) -> Result<MemoryForgetResponse, agent_client_protocol::Error> {
        // See on_memory_flush comment re: Send constraint.
        let body = ForgetBody {
            // The MemoryForgetRequest schema uses `id` which is the chunk
            // identifier ("relative/path.md#chunk-N"). We pass it as the
            // `path` so the proxy can resolve to the file path.
            path: Some(&req.id),
            query: None,
            user_id: None,
        };
        let reply: ForgetReply = post_json("forget", &body).await?;
        tracing::info!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = "forget",
            outcome = if reply.ok { "ok" } else { "error" },
            deleted = reply.deleted,
        );
        Ok(MemoryForgetResponse {
            deleted: reply.deleted,
        })
    }

    /// SPEC-051 AC-2: real `_rusky/memory/status` handler.
    pub async fn on_memory_status(
        &self,
        _req: MemoryStatusRequest,
    ) -> Result<MemoryStatusResponse, agent_client_protocol::Error> {
        // See on_memory_flush comment re: Send constraint.
        let reply: StatusReply = get_json("status").await?;
        tracing::debug!(
            target: "goose::acp::rusky_memory",
            event = "rusky_memory_acp_call",
            method = "status",
            outcome = if reply.ok { "ok" } else { "error" },
            total_entries = reply.total_entries,
            worker_queue_depth = reply.worker_queue_depth,
        );
        Ok(MemoryStatusResponse {
            status: reply.status,
            message: "ok".into(),
            spec: None,
            total_entries: reply.total_entries,
            last_compiled_date: reply.last_compiled_date,
            store_size_bytes: reply.store_size_bytes,
        })
    }
}

// ── /RUSKY FORK PATCH: _rusky/memory/* ───────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit-level tests. End-to-end (ACP → proxy roundtrip) tests live in
    //! the `rusky_memory_acp_proxy` integration suite.
    //!
    //! NOTE: tests in this module touch process-wide env vars and so MUST
    //! serialise via the `ENV_LOCK` static. Cargo runs tests in a single
    //! crate concurrently by default; without the lock, distinct tests
    //! race on the same env var and produce flaky outcomes.

    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// SPEC-051 AC-4: env discovery falls back when unset.
    #[test]
    fn proxy_url_defaults_to_localhost() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var(PROXY_URL_ENV).ok();
        std::env::remove_var(PROXY_URL_ENV);
        assert_eq!(proxy_url(), "http://127.0.0.1:8080");
        if let Some(p) = prev {
            std::env::set_var(PROXY_URL_ENV, p);
        }
    }

    /// SPEC-051 AC-4: missing local token returns -32603, not -32601.
    #[tokio::test]
    async fn missing_local_token_yields_internal_error() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev_url = std::env::var(PROXY_URL_ENV).ok();
        let prev_tok = std::env::var(LOCAL_AGENT_ENV).ok();
        std::env::remove_var(LOCAL_AGENT_ENV);
        // Use a URL that is guaranteed unreachable; the missing-token
        // guard runs before any network call.
        std::env::set_var(PROXY_URL_ENV, "http://127.0.0.1:1");

        let err = post_json::<FlushBody<'_>, FlushReply>(
            "flush",
            &FlushBody {
                session_id: Some("s"),
                flush_text: "",
                last_message_id: 0,
                user_id: None,
            },
        )
        .await
        .expect_err("must err without token");
        let err_json = serde_json::to_value(&err).unwrap();
        // Must be -32603 internal error (NOT -32601 method-not-found).
        assert_eq!(err_json.get("code").and_then(|c| c.as_i64()), Some(-32603));

        if let Some(p) = prev_url {
            std::env::set_var(PROXY_URL_ENV, p);
        } else {
            std::env::remove_var(PROXY_URL_ENV);
        }
        if let Some(p) = prev_tok {
            std::env::set_var(LOCAL_AGENT_ENV, p);
        }
    }
}
