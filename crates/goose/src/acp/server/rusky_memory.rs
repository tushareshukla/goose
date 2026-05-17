// ── RUSKY FORK PATCH: _rusky/memory/* ────────────────────────────────────────
//
// Stub handlers for the _rusky/memory/* ACP method family.
//
// All five methods return -32001 (not-yet-implemented sentinel) until SPEC-040
// ships the real memory-subsystem implementation. The request/response types are
// fully defined in goose-sdk so the TypeScript SDK types are stable before the
// implementation lands.
//
// This module is gated by #[cfg(feature = "rusky-memory")] at the module
// declaration site in server.rs — no inner attribute needed.
//
// See SPEC-051, SPEC-040.

use super::GooseAcpAgent;
use crate::acp::custom_requests::{
    MemoryCompileRequest, MemoryCompileResponse, MemoryFlushRequest, MemoryFlushResponse,
    MemoryForgetRequest, MemoryForgetResponse, MemorySearchRequest, MemorySearchResponse,
    MemoryStatusRequest, MemoryStatusResponse,
};

/// The -32001 sentinel code used for "not yet implemented" stubs.
/// Distinct from -32603 (internal error) and -32601 (method not found) so
/// callers can distinguish "registered stub" from "unregistered method".
const NOT_IMPLEMENTED_CODE: i32 = -32001;
const NOT_IMPLEMENTED_MSG: &str = "not implemented; SPEC-040 ships the real implementation";

fn not_implemented() -> agent_client_protocol::Error {
    agent_client_protocol::Error::new(NOT_IMPLEMENTED_CODE, NOT_IMPLEMENTED_MSG)
        .data(serde_json::json!({ "status": "not_implemented", "spec": "SPEC-040" }))
}

impl GooseAcpAgent {
    /// Entry point called by dispatch_custom_request when rusky-memory is enabled.
    /// Returns Some(result) if the method was handled, None to fall through.
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

    pub async fn on_memory_flush(
        &self,
        _req: MemoryFlushRequest,
    ) -> Result<MemoryFlushResponse, agent_client_protocol::Error> {
        let _span = tracing::info_span!("rusky.memory.flush").entered();
        Err(not_implemented())
    }

    pub async fn on_memory_compile(
        &self,
        _req: MemoryCompileRequest,
    ) -> Result<MemoryCompileResponse, agent_client_protocol::Error> {
        let _span = tracing::info_span!("rusky.memory.compile").entered();
        Err(not_implemented())
    }

    pub async fn on_memory_search(
        &self,
        _req: MemorySearchRequest,
    ) -> Result<MemorySearchResponse, agent_client_protocol::Error> {
        let _span = tracing::info_span!("rusky.memory.search").entered();
        Err(not_implemented())
    }

    pub async fn on_memory_forget(
        &self,
        _req: MemoryForgetRequest,
    ) -> Result<MemoryForgetResponse, agent_client_protocol::Error> {
        let _span = tracing::info_span!("rusky.memory.forget").entered();
        Err(not_implemented())
    }

    pub async fn on_memory_status(
        &self,
        _req: MemoryStatusRequest,
    ) -> Result<MemoryStatusResponse, agent_client_protocol::Error> {
        let _span = tracing::debug_span!("rusky.memory.status").entered();
        Err(not_implemented())
    }
}

// ── /RUSKY FORK PATCH: _rusky/memory/* ───────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_flush_returns_not_implemented() {
        let err = not_implemented();
        let err_json = serde_json::to_value(&err).unwrap();
        // Code must serialize to -32001 (not -32603 internal or -32601 method_not_found)
        let code = err_json.get("code").and_then(|c| c.as_i64());
        assert_eq!(code, Some(NOT_IMPLEMENTED_CODE as i64));
        // data.spec must be "SPEC-040"
        let spec = err_json
            .get("data")
            .and_then(|d| d.get("spec"))
            .and_then(|s| s.as_str());
        assert_eq!(spec, Some("SPEC-040"));
    }

    #[test]
    fn test_not_implemented_code_is_distinct_from_method_not_found() {
        // -32601 is method_not_found; -32001 is our not-yet-implemented sentinel
        assert_ne!(NOT_IMPLEMENTED_CODE, -32601i32);
        // -32603 is internal_error
        assert_ne!(NOT_IMPLEMENTED_CODE, -32603i32);
        assert_eq!(NOT_IMPLEMENTED_CODE, -32001i32);
    }
}
