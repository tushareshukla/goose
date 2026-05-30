// ── RUSKY FORK PATCH: _rusky/coordinator/* ──────────────────────────────────
//
// ACP handlers for the SPEC-070 AI Team tab. These methods are the wedge
// between the rusky-app frontend (`rusky-app/src/features/ai-team/api/aiTeam.ts`)
// and goose's native multi-agent `summon` platform extension
// (`crates/goose/src/agents/platform_extensions/summon.rs`).
//
// Multi-agent vocabulary (rusky-spec.html §05b):
//   - Orchestrator: 1 per session, the lead agent that decomposes & dispatches
//   - Delegate    : researcher | worker | writer | reviewer | coder
//   - Flock       : 3+ delegates coordinating on a shared task
//   - Activity    : live broadcast channel where every delegate posts progress
//   - Telepathy   : direct push from orchestrator to a specific delegate
//   - Crossfire   : adversarial review — 2 reviewers on different models
//
// Methods implemented (all called via `client.extMethod(...)` from the FE):
//   _rusky/coordinator/state/get          — snapshot current AI Team state
//   _rusky/coordinator/events/subscribe   — long-running subscription stub
//   _rusky/coordinator/control/pause      — pause delegate(s)
//   _rusky/coordinator/control/resume     — resume delegate(s)
//   _rusky/coordinator/control/stop       — terminate delegate(s)
//   _rusky/coordinator/control/setCoderTier — set coder-only model tier
//   _rusky/coordinator/telepathy/push     — direct message to delegate
//   _rusky/coordinator/crossfire/start    — start adversarial review
//
// The handlers are intentionally fail-soft: if the FE calls them on a goose
// build that ships them, they always return a valid (possibly empty) state.
// The FE in turn degrades to live event-bus updates when state is empty.
//
// Coder tier is stored in goose Config under `GOOSE_RUSKY_CODER_TIER`
// (mirrors `GOOSE_PROVIDER`/`GOOSE_MODEL` pattern in config.rs). The tier
// is read whenever a coder delegate is about to spawn — see the summon
// extension hook in `summon.rs::build_task_config` (SPEC-001 ties this to
// the OpenRouter `coder` alias on the Rusky proxy).

use super::GooseAcpAgent;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Goose Config key under which the AI Team coder tier is stored. The
/// rusky-proxy reads the same alias from the request and maps it to a
/// pinned OpenRouter model id (see rusky-spec.html §04 tier table).
pub const CODER_TIER_CONFIG_KEY: &str = "GOOSE_RUSKY_CODER_TIER";

const ACTIVITY_BUFFER_CAP: usize = 256;
const TELEPATHY_BUFFER_CAP: usize = 64;

// ─── Wire-format DTOs ───────────────────────────────────────────────────────
//
// Field names use camelCase to match the FE adapter in `aiTeam.ts`. The
// adapter also accepts snake_case fallbacks, but we standardise on camel.

/// One delegate in the active flock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delegate {
    pub id: String,
    pub name: String,
    /// One of: researcher | worker | writer | reviewer | coder
    pub role: String,
    /// One of: idle | thinking | running | needs-review | done | error | paused
    pub status: String,
    #[serde(default)]
    pub task_summary: String,
    /// Only meaningful for `role == "coder"`. Junior | Senior | Principal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

/// One row in the activity feed (or a telepathy/crossfire side-channel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEvent {
    pub id: String,
    pub delegate_id: String,
    pub summary: String,
    pub timestamp: u64,
    /// "activity" | "telepathy" | "crossfire"
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telepathy_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crossfire: Option<CrossfireMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossfireMeta {
    pub draft_id: String,
    pub partner_delegate_id: String,
    pub model: String,
    /// "approve" | "reject" | "uncertain"
    pub verdict: String,
}

/// One-shot snapshot returned by `state/get`.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiTeamState {
    /// One of: idle | active | paused | error
    pub coordinator_status: String,
    pub delegates: Vec<Delegate>,
    pub events: Vec<TimelineEvent>,
}

// ─── In-memory state — process-wide singleton ───────────────────────────────
//
// The summon extension already keeps its own per-session state. The
// coordinator state mirrored here is the *view* the AI Team tab observes —
// an ordered list of delegates, an activity log, and the orchestrator's
// pause/resume bit. It's deliberately decoupled from summon's internal
// background-task map so that handler reads stay sub-millisecond and never
// block on a session-manager mutex.
//
// The bridge from summon → coordinator state will land alongside the
// summon notification hook (SPEC-070 follow-up). For now, the state is
// driven directly by the control & telepathy handlers, and snapshots
// merge in any externally-pushed delegate rows. The FE already degrades
// to "live events only" when the snapshot is empty.

#[derive(Default)]
struct CoordinatorStore {
    coordinator_status: String,
    delegates: Vec<Delegate>,
    events: VecDeque<TimelineEvent>,
    telepathy_queue: VecDeque<TimelineEvent>,
    seq: u64,
}

impl CoordinatorStore {
    fn new() -> Self {
        Self {
            coordinator_status: "idle".to_string(),
            delegates: Vec::new(),
            events: VecDeque::with_capacity(ACTIVITY_BUFFER_CAP),
            telepathy_queue: VecDeque::with_capacity(TELEPATHY_BUFFER_CAP),
            seq: 0,
        }
    }

    fn next_event_id(&mut self, prefix: &str) -> String {
        self.seq = self.seq.wrapping_add(1);
        format!("evt-{}-{}-{}", prefix, current_epoch_millis(), self.seq)
    }

    fn push_event(&mut self, event: TimelineEvent) {
        if self.events.len() >= ACTIVITY_BUFFER_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn push_telepathy(&mut self, event: TimelineEvent) {
        if self.telepathy_queue.len() >= TELEPATHY_BUFFER_CAP {
            self.telepathy_queue.pop_front();
        }
        self.telepathy_queue.push_back(event.clone());
        self.push_event(event);
    }

    fn snapshot(&self) -> AiTeamState {
        AiTeamState {
            coordinator_status: self.coordinator_status.clone(),
            delegates: self.delegates.clone(),
            events: self.events.iter().cloned().collect(),
        }
    }

    fn set_delegate_status(&mut self, id: Option<&str>, status: &str) {
        match id {
            Some(target) => {
                for d in self.delegates.iter_mut() {
                    if d.id == target {
                        d.status = status.to_string();
                    }
                }
            }
            None => {
                for d in self.delegates.iter_mut() {
                    d.status = status.to_string();
                }
            }
        }
    }
}

fn store() -> &'static Mutex<CoordinatorStore> {
    static STORE: OnceLock<Mutex<CoordinatorStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(CoordinatorStore::new()))
}

fn current_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ─── State helpers — exposed for the integration test ──────────────────────

/// Reset the process-wide coordinator store. Used by the integration suite
/// to make tests order-independent.
pub async fn reset_store_for_test() {
    let mut s = store().lock().await;
    *s = CoordinatorStore::new();
}

/// Seed one delegate so the control-method tests have something to
/// mutate. Idempotent on `id`.
pub async fn seed_delegate_for_test(delegate: Delegate) {
    let mut s = store().lock().await;
    if !s.delegates.iter().any(|d| d.id == delegate.id) {
        s.delegates.push(delegate);
    }
    s.coordinator_status = "active".to_string();
}

// ─── Request DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlRequest {
    /// One specific delegate to pause/resume/stop.
    #[serde(default)]
    delegate_id: Option<String>,
    /// "all" → applies to every delegate. Mutually-exclusive with delegate_id.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetCoderTierRequest {
    /// FE always sends a `delegateId` but at proxy-time the tier is global
    /// — see SPEC-001. We accept it and log it, then write the global key.
    #[serde(default)]
    delegate_id: Option<String>,
    tier: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelepathyPushRequest {
    delegate_id: String,
    message: String,
    /// Optional sender id; defaults to "orchestrator".
    #[serde(default)]
    from: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CrossfireStartRequest {
    draft_id: String,
    /// Two reviewer delegate ids that will check the same draft.
    reviewer_ids: Vec<String>,
    /// Optional human-readable model labels for each reviewer.
    #[serde(default)]
    models: Vec<String>,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

const VALID_TIERS: &[&str] = &["junior", "senior", "principal"];

fn normalize_tier(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if !VALID_TIERS.iter().any(|t| *t == lower) {
        return None;
    }
    Some(lower)
}

fn invalid<T: Into<String>>(msg: T) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(msg.into())
}

fn internal<T: Into<String>>(msg: T) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(msg.into())
}

fn control_target(req: &ControlRequest) -> Result<Option<String>, agent_client_protocol::Error> {
    match (&req.delegate_id, &req.scope) {
        (Some(id), _) if !id.trim().is_empty() => Ok(Some(id.clone())),
        (Some(_), _) => Err(invalid("delegateId must not be empty")),
        (None, Some(scope)) if scope == "all" => Ok(None),
        (None, None) => Ok(None),
        (None, Some(other)) => Err(invalid(format!(
            "scope must be 'all' when delegateId is omitted, got '{other}'"
        ))),
    }
}

// ─── Free-function handlers ─────────────────────────────────────────────────
//
// These do not need an agent reference; they own state via the process-wide
// `store()` singleton. The integration suite in
// `goose/tests/rusky_coordinator_acp.rs` calls them directly.

/// SPEC-070 AC-1: snapshot the current AI Team state.
pub async fn handle_coordinator_state(
    _params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let s = store().lock().await;
    let snap = s.snapshot();
    tracing::debug!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "state/get",
        delegates = snap.delegates.len(),
        events = snap.events.len(),
        "snapshot returned",
    );
    serde_json::to_value(&snap).map_err(|e| internal(format!("serialize state: {e}")))
}

/// SPEC-070 AC-2: events/subscribe.
///
/// ACP's long-running subscription semantics are not yet wired through
/// `extMethod`, so the FE actually consumes the live stream via the
/// `sessionUpdate` notification bus (subagent-tagged updates from the
/// summon extension). This handler returns the current buffered events
/// so the FE has something to render after a subscribe call until the
/// real bridge ships. The shape mirrors `state/get` minus the delegates.
pub async fn handle_coordinator_events_subscribe(
    _params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let s = store().lock().await;
    let events: Vec<TimelineEvent> = s.events.iter().cloned().collect();
    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "events/subscribe",
        buffered = events.len(),
    );
    Ok(serde_json::json!({
        "events": events,
        "streamingTransport": "session-update",
    }))
}

/// SPEC-070 AC-3: pause delegate or whole flock.
pub async fn handle_control_pause(
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let req: ControlRequest =
        serde_json::from_value(params).map_err(|e| invalid(format!("bad control body: {e}")))?;
    let target = control_target(&req)?;
    let mut s = store().lock().await;
    s.set_delegate_status(target.as_deref(), "paused");
    if target.is_none() {
        s.coordinator_status = "paused".to_string();
    }
    let id = s.next_event_id("pause");
    let summary = match &target {
        Some(t) => format!("Paused delegate {t}"),
        None => "Paused entire flock".to_string(),
    };
    s.push_event(TimelineEvent {
        id,
        delegate_id: target.clone().unwrap_or_else(|| "orchestrator".to_string()),
        summary,
        timestamp: current_epoch_millis(),
        channel: "activity".to_string(),
        telepathy_target_id: None,
        crossfire: None,
    });
    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "control/pause",
        scope = target.as_deref().unwrap_or("all"),
    );
    Ok(serde_json::json!({ "ok": true }))
}

/// SPEC-070 AC-4: resume a paused delegate or the whole flock.
pub async fn handle_control_resume(
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let req: ControlRequest =
        serde_json::from_value(params).map_err(|e| invalid(format!("bad control body: {e}")))?;
    let target = control_target(&req)?;
    let mut s = store().lock().await;
    s.set_delegate_status(target.as_deref(), "running");
    if target.is_none() {
        s.coordinator_status = "active".to_string();
    }
    let id = s.next_event_id("resume");
    let summary = match &target {
        Some(t) => format!("Resumed delegate {t}"),
        None => "Resumed entire flock".to_string(),
    };
    s.push_event(TimelineEvent {
        id,
        delegate_id: target.clone().unwrap_or_else(|| "orchestrator".to_string()),
        summary,
        timestamp: current_epoch_millis(),
        channel: "activity".to_string(),
        telepathy_target_id: None,
        crossfire: None,
    });
    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "control/resume",
        scope = target.as_deref().unwrap_or("all"),
    );
    Ok(serde_json::json!({ "ok": true }))
}

/// SPEC-070 AC-5: stop in-flight delegate(s).
///
/// The actual underlying cancellation lives in the summon extension
/// (`SummonClient::handle_load_task_result` with `cancel: true`). When
/// the FE wires this through, the handler maps `delegate_id` →
/// summon task id and triggers cancellation; until then, we mutate
/// the mirror state so the UI reflects the intent.
pub async fn handle_control_stop(
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let req: ControlRequest =
        serde_json::from_value(params).map_err(|e| invalid(format!("bad control body: {e}")))?;
    let target = control_target(&req)?;
    let mut s = store().lock().await;
    s.set_delegate_status(target.as_deref(), "done");
    if target.is_none() {
        s.coordinator_status = "idle".to_string();
        s.delegates.clear();
    } else if let Some(t) = target.as_deref() {
        s.delegates.retain(|d| d.id != t);
    }
    let id = s.next_event_id("stop");
    let summary = match &target {
        Some(t) => format!("Stopped delegate {t}"),
        None => "Stopped entire flock".to_string(),
    };
    s.push_event(TimelineEvent {
        id,
        delegate_id: target.clone().unwrap_or_else(|| "orchestrator".to_string()),
        summary,
        timestamp: current_epoch_millis(),
        channel: "activity".to_string(),
        telepathy_target_id: None,
        crossfire: None,
    });
    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "control/stop",
        scope = target.as_deref().unwrap_or("all"),
    );
    Ok(serde_json::json!({ "ok": true }))
}

/// SPEC-070 AC-7: orchestrator pushes a direct message ("telepathy") to one
/// delegate. Recorded in both the activity feed and the dedicated
/// telepathy queue so the FE can render the side-channel separately.
pub async fn handle_telepathy_push(
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let req: TelepathyPushRequest =
        serde_json::from_value(params).map_err(|e| invalid(format!("bad telepathy body: {e}")))?;
    if req.delegate_id.trim().is_empty() {
        return Err(invalid("delegateId must not be empty"));
    }
    if req.message.trim().is_empty() {
        return Err(invalid("message must not be empty"));
    }

    let mut s = store().lock().await;
    let id = s.next_event_id("telepathy");
    let event = TimelineEvent {
        id: id.clone(),
        delegate_id: req
            .from
            .clone()
            .unwrap_or_else(|| "orchestrator".to_string()),
        summary: req.message.clone(),
        timestamp: current_epoch_millis(),
        channel: "telepathy".to_string(),
        telepathy_target_id: Some(req.delegate_id.clone()),
        crossfire: None,
    };
    s.push_telepathy(event);

    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "telepathy/push",
        target = %req.delegate_id,
    );
    Ok(serde_json::json!({ "ok": true, "eventId": id }))
}

/// SPEC-070 AC-8: start an adversarial review. Records a crossfire-channel
/// event tagging both reviewers so the UI can group them visually.
pub async fn handle_crossfire_start(
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let req: CrossfireStartRequest =
        serde_json::from_value(params).map_err(|e| invalid(format!("bad crossfire body: {e}")))?;
    if req.reviewer_ids.len() < 2 {
        return Err(invalid(
            "crossfire requires at least 2 reviewer ids (different models)",
        ));
    }

    let mut s = store().lock().await;
    let id = s.next_event_id("crossfire");
    // The first reviewer authors the event; the partner is the second.
    let primary = req.reviewer_ids[0].clone();
    let partner = req.reviewer_ids[1].clone();
    let model = req.models.first().cloned().unwrap_or_default();
    let crossfire = CrossfireMeta {
        draft_id: req.draft_id.clone(),
        partner_delegate_id: partner.clone(),
        model,
        verdict: "uncertain".to_string(),
    };
    s.push_event(TimelineEvent {
        id: id.clone(),
        delegate_id: primary.clone(),
        summary: format!("Crossfire review started on draft {}", req.draft_id),
        timestamp: current_epoch_millis(),
        channel: "crossfire".to_string(),
        telepathy_target_id: None,
        crossfire: Some(crossfire),
    });

    tracing::info!(
        target: "goose::acp::rusky_coordinator",
        event = "rusky_coordinator_acp_call",
        method = "crossfire/start",
        draft = %req.draft_id,
        reviewers = req.reviewer_ids.len(),
    );
    Ok(serde_json::json!({
        "ok": true,
        "eventId": id,
        "primaryReviewerId": primary,
        "partnerReviewerId": partner,
    }))
}

/// Mirror the new tier on any tracked coder delegate so the FE re-renders
/// the tier chip without waiting for the next snapshot. Logs a feed line.
async fn record_coder_tier_change(delegate_id: Option<&str>, tier: &str) {
    let mut s = store().lock().await;
    for d in s.delegates.iter_mut() {
        if d.role == "coder" {
            match delegate_id {
                Some(target) if d.id == target => d.tier = Some(tier.to_string()),
                None => d.tier = Some(tier.to_string()),
                _ => {}
            }
        }
    }
    let id = s.next_event_id("tier");
    let delegate_id = delegate_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| "orchestrator".to_string());
    s.push_event(TimelineEvent {
        id,
        delegate_id,
        summary: format!("Coder tier set to {}", tier),
        timestamp: current_epoch_millis(),
        channel: "activity".to_string(),
        telepathy_target_id: None,
        crossfire: None,
    });
}

// ─── ACP dispatch ───────────────────────────────────────────────────────────

impl GooseAcpAgent {
    /// Entry point called by `dispatch_custom_request`. Returns `Some(result)`
    /// when the method is in the `_rusky/coordinator/*` family, `None` to
    /// fall through to the typed-method dispatcher.
    pub async fn dispatch_rusky_coordinator_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
        let result = match method {
            "_rusky/coordinator/state/get" | "_rusky/coordinator/state" => {
                handle_coordinator_state(params).await
            }
            "_rusky/coordinator/events/subscribe" => {
                handle_coordinator_events_subscribe(params).await
            }
            "_rusky/coordinator/control/pause" => handle_control_pause(params).await,
            "_rusky/coordinator/control/resume" => handle_control_resume(params).await,
            "_rusky/coordinator/control/stop" => handle_control_stop(params).await,
            "_rusky/coordinator/control/setCoderTier" => {
                self.on_control_set_coder_tier(params).await
            }
            "_rusky/coordinator/telepathy/push" => handle_telepathy_push(params).await,
            "_rusky/coordinator/crossfire/start" => handle_crossfire_start(params).await,
            _ => return None,
        };
        Some(result)
    }

    /// SPEC-070 AC-6 / SPEC-001: store the AI-Team coder tier as a goose
    /// preference. Validates the tier label, writes
    /// `GOOSE_RUSKY_CODER_TIER` to the config, and emits an activity-feed
    /// line so the UI sees the change without waiting for the next poll.
    async fn on_control_set_coder_tier(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        let req: SetCoderTierRequest = serde_json::from_value(params)
            .map_err(|e| invalid(format!("bad setCoderTier body: {e}")))?;
        let Some(tier) = normalize_tier(&req.tier) else {
            return Err(invalid(format!(
                "tier must be one of Junior|Senior|Principal, got '{}'",
                req.tier
            )));
        };

        let config = self.config()?;
        config
            .set_param(CODER_TIER_CONFIG_KEY, &tier)
            .map_err(|e| internal(format!("save coder tier: {e}")))?;

        record_coder_tier_change(req.delegate_id.as_deref(), &tier).await;

        tracing::info!(
            target: "goose::acp::rusky_coordinator",
            event = "rusky_coordinator_acp_call",
            method = "control/setCoderTier",
            tier = %tier,
        );
        Ok(serde_json::json!({ "ok": true, "tier": tier }))
    }
}

// ── /RUSKY FORK PATCH: _rusky/coordinator/* ─────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for the in-module helpers. End-to-end ACP-method tests
    //! live in `goose/tests/rusky_coordinator_acp.rs`.

    use super::*;

    #[test]
    fn normalize_tier_accepts_canonical_labels() {
        assert_eq!(normalize_tier("Junior"), Some("junior".into()));
        assert_eq!(normalize_tier("senior"), Some("senior".into()));
        assert_eq!(normalize_tier("PRINCIPAL"), Some("principal".into()));
    }

    #[test]
    fn normalize_tier_rejects_unknown() {
        assert_eq!(normalize_tier(""), None);
        assert_eq!(normalize_tier("staff"), None);
        assert_eq!(normalize_tier("  "), None);
    }

    #[test]
    fn control_target_prefers_delegate_id() {
        let req = ControlRequest {
            delegate_id: Some("d-1".into()),
            scope: Some("all".into()),
        };
        assert_eq!(control_target(&req).unwrap(), Some("d-1".into()));
    }

    #[test]
    fn control_target_all_when_scope_all() {
        let req = ControlRequest {
            delegate_id: None,
            scope: Some("all".into()),
        };
        assert_eq!(control_target(&req).unwrap(), None);
    }

    #[test]
    fn control_target_rejects_unknown_scope() {
        let req = ControlRequest {
            delegate_id: None,
            scope: Some("flock".into()),
        };
        assert!(control_target(&req).is_err());
    }

    #[test]
    fn timeline_event_serializes_camel_case() {
        let ev = TimelineEvent {
            id: "evt-1".into(),
            delegate_id: "d-1".into(),
            summary: "hi".into(),
            timestamp: 1000,
            channel: "telepathy".into(),
            telepathy_target_id: Some("d-2".into()),
            crossfire: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        // FE's adapter pins delegateId / telepathyTargetId (camelCase).
        assert!(v.get("delegateId").is_some());
        assert!(v.get("telepathyTargetId").is_some());
        assert!(v.get("delegate_id").is_none());
    }
}
