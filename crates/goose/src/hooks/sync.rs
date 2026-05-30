//! Synchronous lifecycle hooks (Rusky fork extension).
//!
//! The base `hooks/mod.rs` ships a fire-and-forget observer model: when an
//! event fires, registered shell-command actions run in the background and
//! cannot influence the in-flight operation. That model is sufficient for
//! observability (logging, metric emission, side-effect plug-ins) but it is
//! NOT sufficient for the Rusky memory subsystem (SPEC-040), which must:
//!
//! 1. Stop the LLM dispatch BEFORE the request is sent.
//! 2. Inject relevant memories as a system message (Recall).
//! 3. Flush in-flight turns to the daily log BEFORE auto-compaction reduces
//!    the conversation history (Flush).
//!
//! This module introduces a new `SyncHook` trait and a `SyncHookManager` that
//! the agent loop awaits at well-defined insertion points. The existing
//! observer-style `HookManager` is left untouched and continues to operate
//! exactly as before. Vanilla `goose serve` with no Rusky hooks registered
//! sees this code as a single `is_empty()` check per turn — no perf regression.
//!
//! ## Backwards compatibility
//!
//! - No existing public API is modified.
//! - Default `SyncHookManager::default()` has zero hooks; `dispatch_*` is a
//!   one-Vec-is-empty branch.
//! - Existing fire-and-forget `HookManager::emit` calls in goose continue to
//!   work unchanged.
//!
//! ## Glossary
//!
//! - **SyncHook** — A hook that the agent loop AWAITS before continuing. May
//!   mutate the in-flight request (PreLLMRequest) or signal abort.
//! - **PreLLMRequest** — Fires immediately before the agent dispatches a
//!   completion call to its provider. Hooks may prepend / append system
//!   messages.
//! - **PreCompact** — Fires immediately before `compact_messages` reduces the
//!   conversation history. Hooks may flush pending writes; mutations to the
//!   conversation are not supported here (the compactor owns it).
//! - **2s hook timeout** — Each individual hook execution is bounded by a
//!   2-second timeout. Exceeding logs WARN, ignores the hook's contribution,
//!   and continues the chain. Rusky's memory recall budget is 200ms p99
//!   (SPEC-040 §Performance budget); 2s is a generous ceiling.
//!
//! SPEC-051 — sync hook patches.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Default per-hook execution timeout. Rusky memory recall p99 budget is
/// 200ms (SPEC-040 §Performance budget); 2s leaves an order of magnitude
/// of headroom for proxy round-trips and degraded paths.
pub const SYNC_HOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Events that synchronous hooks can subscribe to.
///
/// Distinct from `HookEvent` in `hooks/mod.rs` because:
///
/// - The execution semantics differ (awaited + mutable vs fire-and-forget).
/// - The payload differs (`SyncHookContext` carries the conversation, not a
///   serialized JSON envelope).
/// - Mixing the enums would force every plugin-shell-command hook to handle
///   the mutable variants (or vice versa), which is a footgun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncHookEvent {
    /// Fires before the agent dispatches an LLM completion call.
    /// Hooks may inject system messages (memory recall) or abort.
    PreLLMRequest,
    /// Fires before `compact_messages` reduces conversation history.
    /// Hooks may flush pending writes; the conversation is read-only here.
    PreCompact,
}

impl SyncHookEvent {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::PreLLMRequest => "PreLLMRequest",
            Self::PreCompact => "PreCompact",
        }
    }
}

/// Context passed to a sync hook.
///
/// Fields are option-typed so hooks can be reused across events without
/// breaking when an event does not carry every field.
#[derive(Debug, Default, Clone)]
pub struct SyncHookContext {
    pub session_id: String,
    /// PreLLMRequest only: the messages array about to be sent. The hook
    /// returns mutations rather than modifying in place; the manager applies
    /// them in registration order so the order is deterministic.
    pub messages_summary: Option<String>,
    /// PreLLMRequest only: the system prompt. Hooks read this to decide
    /// recall scope; they do not return a mutated system prompt — that's the
    /// job of the system-message injection in the mutation field.
    pub system_prompt: Option<String>,
}

/// A single mutation a hook can return.
///
/// Modeled as data, not as a closure, so the SyncHookManager can log what
/// each hook did before applying — critical for diagnosing why a recall
/// injection landed (or didn't).
#[derive(Debug, Clone)]
pub enum SyncHookMutation {
    /// Insert a system message at position N in the messages array. The
    /// agent loop translates this to a real `Message` insert at the
    /// appropriate index. For Rusky's memory recall, hooks typically emit
    /// `PrependSystemMessage` to inject context before the user's turn.
    PrependSystemMessage(String),
    /// Append a system message at the end (less common; primarily for
    /// trailing reminders).
    AppendSystemMessage(String),
}

/// What a sync hook returns from its `on_event` callback.
#[derive(Debug, Clone)]
pub enum SyncHookOutcome {
    /// Continue the chain. The (possibly empty) mutations are applied.
    Continue { mutations: Vec<SyncHookMutation> },
    /// Abort the whole operation. The agent loop yields an error to the
    /// caller and does not dispatch the LLM call / compaction. The string
    /// is logged at WARN and is safe to surface to the user.
    Abort { reason: String },
}

impl SyncHookOutcome {
    /// Helper for hooks that don't need mutations.
    #[must_use]
    pub fn cont() -> Self {
        Self::Continue {
            mutations: Vec::new(),
        }
    }

    /// Helper to build a `Continue` with one prepended system message.
    #[must_use]
    pub fn inject(text: impl Into<String>) -> Self {
        Self::Continue {
            mutations: vec![SyncHookMutation::PrependSystemMessage(text.into())],
        }
    }
}

/// The trait that Rusky-side code (and any future plugin author) implements
/// to participate in synchronous hook events.
///
/// `Send + Sync + 'static`: hooks are stored behind `Arc` and may be invoked
/// from any tokio worker thread.
#[async_trait]
pub trait SyncHook: Send + Sync + 'static {
    /// Human-readable name used in structured tracing events.
    fn name(&self) -> &'static str;

    /// Which events this hook is interested in. The manager only invokes
    /// `on_event` for events listed here.
    fn events(&self) -> &'static [SyncHookEvent];

    /// Invoked by the manager when one of the hook's `events()` fires.
    /// The implementation MUST return within `SYNC_HOOK_TIMEOUT`; the manager
    /// races the future against the timeout and, on timeout, logs WARN and
    /// treats the outcome as `Continue { mutations: vec![] }` so the chain
    /// keeps moving.
    async fn on_event(
        &self,
        event: SyncHookEvent,
        ctx: &SyncHookContext,
    ) -> anyhow::Result<SyncHookOutcome>;
}

/// Aggregated outcome of a hook chain. Returned to the agent loop so it
/// knows whether to proceed and what to splice into the conversation.
#[derive(Debug, Default, Clone)]
pub struct SyncHookChainResult {
    /// All mutations from all `Continue` outcomes, in registration order.
    pub mutations: Vec<SyncHookMutation>,
    /// If any hook aborted, this is `Some(reason)`. Mutations from earlier
    /// hooks are still present — the caller decides whether to apply them
    /// (Rusky's policy: skip everything on abort).
    pub aborted: Option<String>,
}

impl SyncHookChainResult {
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted.is_some()
    }
}

/// Registry of sync hooks, indexed for fast `dispatch`.
///
/// Cloning is cheap: the manager wraps the registration list in an `Arc`.
/// Hot path (no registered hooks): single `is_empty()` check.
#[derive(Default, Clone)]
pub struct SyncHookManager {
    inner: Arc<Mutex<Vec<Arc<dyn SyncHook>>>>,
}

impl std::fmt::Debug for SyncHookManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't try to lock from Debug — that's a deadlock hazard. Just
        // signal that this is a SyncHookManager; the contents are runtime
        // state, not user-facing config.
        f.debug_struct("SyncHookManager").finish_non_exhaustive()
    }
}

impl SyncHookManager {
    /// Build an empty manager. Identical to `Default::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook. Order of registration is preserved across dispatch.
    pub async fn register(&self, hook: Arc<dyn SyncHook>) {
        let name = hook.name();
        let events: Vec<&'static str> = hook.events().iter().map(SyncHookEvent::name).collect();
        let mut guard = self.inner.lock().await;
        guard.push(hook);
        info!(
            target: "goose::hooks::sync",
            event = "prellm_hook_registered",
            hook = name,
            events = ?events,
            total_registered = guard.len(),
            "sync hook registered",
        );
    }

    /// Returns true if no hooks are registered for any event.
    ///
    /// Used by the agent loop to short-circuit dispatch on the hot path.
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// Cheap synchronous snapshot — returns true if there are NO hooks
    /// registered at all. This is the fast path used by the agent loop's
    /// inner per-turn check; it `try_lock`s and conservatively reports
    /// "non-empty" when contended (which makes the next call await once).
    #[must_use]
    pub fn is_empty_hint(&self) -> bool {
        self.inner.try_lock().is_ok_and(|g| g.is_empty())
    }

    /// Fire `event` and await all registered hooks. Each hook is bounded by
    /// `SYNC_HOOK_TIMEOUT`; on timeout the hook's contribution is dropped
    /// with a WARN and the chain continues.
    ///
    /// Hooks run sequentially in registration order so the order of
    /// applied mutations is deterministic (critical: a recall hook
    /// prepending memories MUST run before any hook that depends on them
    /// being present).
    pub async fn dispatch(
        &self,
        event: SyncHookEvent,
        ctx: &SyncHookContext,
    ) -> SyncHookChainResult {
        let hooks = {
            let guard = self.inner.lock().await;
            if guard.is_empty() {
                return SyncHookChainResult::default();
            }
            guard.clone()
        };

        let mut result = SyncHookChainResult::default();

        for hook in hooks {
            if !hook.events().contains(&event) {
                continue;
            }
            let hook_name = hook.name();
            debug!(
                target: "goose::hooks::sync",
                event = "prellm_hook_fired",
                hook = hook_name,
                kind = event.name(),
                "dispatching sync hook",
            );

            let fut = hook.on_event(event, ctx);
            match tokio::time::timeout(SYNC_HOOK_TIMEOUT, fut).await {
                Ok(Ok(SyncHookOutcome::Continue { mutations })) => {
                    let n = mutations.len();
                    result.mutations.extend(mutations);
                    debug!(
                        target: "goose::hooks::sync",
                        hook = hook_name,
                        kind = event.name(),
                        mutations = n,
                        "sync hook continued",
                    );
                }
                Ok(Ok(SyncHookOutcome::Abort { reason })) => {
                    warn!(
                        target: "goose::hooks::sync",
                        event = "prellm_hook_aborted",
                        hook = hook_name,
                        kind = event.name(),
                        reason = %reason,
                        "sync hook aborted chain",
                    );
                    result.aborted = Some(reason);
                    return result;
                }
                Ok(Err(err)) => {
                    warn!(
                        target: "goose::hooks::sync",
                        hook = hook_name,
                        kind = event.name(),
                        error = %err,
                        "sync hook returned error; dropping its contribution",
                    );
                }
                Err(_elapsed) => {
                    warn!(
                        target: "goose::hooks::sync",
                        event = "prellm_hook_timeout",
                        hook = hook_name,
                        kind = event.name(),
                        timeout_ms = SYNC_HOOK_TIMEOUT.as_millis() as u64,
                        "sync hook exceeded timeout; dropping its contribution",
                    );
                }
            }
        }

        if event == SyncHookEvent::PreCompact && !result.mutations.is_empty() {
            // PreCompact does not support mutations (the compactor owns the
            // conversation). Drop them with a WARN so a misconfigured hook
            // surfaces loudly.
            warn!(
                target: "goose::hooks::sync",
                event = "precompact_hook_fired",
                ignored_mutations = result.mutations.len(),
                "PreCompact returned mutations; dropping (PreCompact is read-only)",
            );
            result.mutations.clear();
        }

        result
    }
}

// ─── Process-wide global manager (Rusky integration helper) ──────────────────
//
// The agent loop in `agents/agent.rs` and the compactor in
// `context_mgmt/mod.rs` are upstream goose code we want to touch as little as
// possible. Threading a `SyncHookManager` through every agent constructor
// would scatter Rusky-specific arguments across the goose tree.
//
// Instead, the integration points call into `global()` once per dispatch.
// Default state: an empty manager → a single `Vec::is_empty()` check → zero
// perf cost when no Rusky hooks are registered (i.e. vanilla `goose serve`).
// Rusky-side code (the proxy or rusky-app launcher) registers hooks at
// process start via `global().register(...)`.

use once_cell::sync::Lazy;

static GLOBAL: Lazy<SyncHookManager> = Lazy::new(SyncHookManager::default);

/// Return the process-wide `SyncHookManager` singleton.
///
/// Rusky-side code registers hooks here at process start. The agent loop
/// dispatches into this manager from the two integration points.
#[must_use]
pub fn global() -> &'static SyncHookManager {
    &GLOBAL
}

/// Convenience: dispatch `PreLLMRequest` against the global manager.
/// Returns the chain result. The agent loop calls this once per turn.
pub async fn dispatch_prellm_request(ctx: &SyncHookContext) -> SyncHookChainResult {
    if GLOBAL.is_empty_hint() {
        return SyncHookChainResult::default();
    }
    GLOBAL.dispatch(SyncHookEvent::PreLLMRequest, ctx).await
}

/// Convenience: dispatch `PreCompact` against the global manager.
/// The agent loop calls this immediately before `compact_messages`.
pub async fn dispatch_precompact(ctx: &SyncHookContext) -> SyncHookChainResult {
    if GLOBAL.is_empty_hint() {
        return SyncHookChainResult::default();
    }
    GLOBAL.dispatch(SyncHookEvent::PreCompact, ctx).await
}

#[cfg(test)]
mod tests {
    //! SPEC-051 AC-1 coverage:
    //! - Empty manager is a no-op (`empty_manager_dispatch_is_noop`).
    //! - Mutations from `Continue` are applied in registration order
    //!   (`mutations_applied_in_registration_order`).
    //! - `Abort` short-circuits the chain (`abort_short_circuits_chain`).
    //! - A slow hook is bounded by `SYNC_HOOK_TIMEOUT`
    //!   (`slow_hook_times_out_chain_continues`).
    //! - PreCompact drops mutations (`precompact_mutations_dropped`).
    //! - Hook events filter (`hook_only_invoked_for_its_events`).

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct InjectHook {
        msg: &'static str,
    }

    #[async_trait]
    impl SyncHook for InjectHook {
        fn name(&self) -> &'static str {
            "inject"
        }
        fn events(&self) -> &'static [SyncHookEvent] {
            &[SyncHookEvent::PreLLMRequest]
        }
        async fn on_event(
            &self,
            _event: SyncHookEvent,
            _ctx: &SyncHookContext,
        ) -> anyhow::Result<SyncHookOutcome> {
            Ok(SyncHookOutcome::inject(self.msg.to_string()))
        }
    }

    struct AbortHook;

    #[async_trait]
    impl SyncHook for AbortHook {
        fn name(&self) -> &'static str {
            "abort"
        }
        fn events(&self) -> &'static [SyncHookEvent] {
            &[SyncHookEvent::PreLLMRequest]
        }
        async fn on_event(
            &self,
            _e: SyncHookEvent,
            _c: &SyncHookContext,
        ) -> anyhow::Result<SyncHookOutcome> {
            Ok(SyncHookOutcome::Abort {
                reason: "stop".into(),
            })
        }
    }

    struct SlowHook;

    #[async_trait]
    impl SyncHook for SlowHook {
        fn name(&self) -> &'static str {
            "slow"
        }
        fn events(&self) -> &'static [SyncHookEvent] {
            &[SyncHookEvent::PreLLMRequest]
        }
        async fn on_event(
            &self,
            _e: SyncHookEvent,
            _c: &SyncHookContext,
        ) -> anyhow::Result<SyncHookOutcome> {
            // Sleep longer than SYNC_HOOK_TIMEOUT (2s).
            tokio::time::sleep(SYNC_HOOK_TIMEOUT + Duration::from_millis(200)).await;
            Ok(SyncHookOutcome::inject("never delivered"))
        }
    }

    struct CountingHook {
        counter: Arc<AtomicUsize>,
        accept: SyncHookEvent,
    }

    #[async_trait]
    impl SyncHook for CountingHook {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn events(&self) -> &'static [SyncHookEvent] {
            // Returning a single-entry slice is a SyncHook contract;
            // CountingHook captures `accept` and we filter manually below
            // for the test's purpose: we tell the manager we care about
            // PreLLMRequest only, and assert PreCompact never fires us.
            &[SyncHookEvent::PreLLMRequest]
        }
        async fn on_event(
            &self,
            _e: SyncHookEvent,
            _c: &SyncHookContext,
        ) -> anyhow::Result<SyncHookOutcome> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(_e, self.accept, "manager invoked wrong event");
            Ok(SyncHookOutcome::cont())
        }
    }

    /// SPEC-051 AC-1: default-empty manager is a no-op.
    #[tokio::test]
    async fn empty_manager_dispatch_is_noop() {
        let mgr = SyncHookManager::new();
        let ctx = SyncHookContext::default();
        let result = mgr.dispatch(SyncHookEvent::PreLLMRequest, &ctx).await;
        assert!(result.mutations.is_empty());
        assert!(!result.is_aborted());
        assert!(mgr.is_empty().await);
        assert!(mgr.is_empty_hint());
    }

    /// SPEC-051 AC-1: mutations from `Continue` are applied in
    /// registration order.
    #[tokio::test]
    async fn mutations_applied_in_registration_order() {
        let mgr = SyncHookManager::new();
        mgr.register(Arc::new(InjectHook { msg: "first" })).await;
        mgr.register(Arc::new(InjectHook { msg: "second" })).await;
        let result = mgr
            .dispatch(SyncHookEvent::PreLLMRequest, &SyncHookContext::default())
            .await;
        assert_eq!(result.mutations.len(), 2);
        match (&result.mutations[0], &result.mutations[1]) {
            (
                SyncHookMutation::PrependSystemMessage(a),
                SyncHookMutation::PrependSystemMessage(b),
            ) => {
                assert_eq!(a, "first");
                assert_eq!(b, "second");
            }
            _ => panic!("expected PrependSystemMessage mutations"),
        }
    }

    /// SPEC-051 AC-1: `Abort` short-circuits the chain.
    #[tokio::test]
    async fn abort_short_circuits_chain() {
        let mgr = SyncHookManager::new();
        mgr.register(Arc::new(InjectHook { msg: "before" })).await;
        mgr.register(Arc::new(AbortHook)).await;
        mgr.register(Arc::new(InjectHook { msg: "after" })).await;
        let result = mgr
            .dispatch(SyncHookEvent::PreLLMRequest, &SyncHookContext::default())
            .await;
        assert!(result.is_aborted());
        // Mutations from `before` are present; `after` is never invoked.
        assert_eq!(result.mutations.len(), 1);
    }

    /// SPEC-051 AC-1: a hook that exceeds `SYNC_HOOK_TIMEOUT` is dropped;
    /// the chain continues.
    #[tokio::test]
    async fn slow_hook_times_out_chain_continues() {
        let mgr = SyncHookManager::new();
        mgr.register(Arc::new(SlowHook)).await;
        mgr.register(Arc::new(InjectHook {
            msg: "after timeout",
        }))
        .await;
        let start = std::time::Instant::now();
        let result = mgr
            .dispatch(SyncHookEvent::PreLLMRequest, &SyncHookContext::default())
            .await;
        let elapsed = start.elapsed();
        // The slow hook was killed at SYNC_HOOK_TIMEOUT (2s), then the
        // fast hook ran promptly. Total should be just over 2s.
        assert!(elapsed < SYNC_HOOK_TIMEOUT + Duration::from_millis(500));
        assert_eq!(result.mutations.len(), 1);
        assert!(!result.is_aborted());
    }

    /// SPEC-051 AC-2: PreCompact drops any returned mutations (read-only).
    #[tokio::test]
    async fn precompact_mutations_dropped() {
        struct PCMutate;
        #[async_trait]
        impl SyncHook for PCMutate {
            fn name(&self) -> &'static str {
                "pc-mut"
            }
            fn events(&self) -> &'static [SyncHookEvent] {
                &[SyncHookEvent::PreCompact]
            }
            async fn on_event(
                &self,
                _e: SyncHookEvent,
                _c: &SyncHookContext,
            ) -> anyhow::Result<SyncHookOutcome> {
                Ok(SyncHookOutcome::inject("attempt to mutate during compact"))
            }
        }
        let mgr = SyncHookManager::new();
        mgr.register(Arc::new(PCMutate)).await;
        let result = mgr
            .dispatch(SyncHookEvent::PreCompact, &SyncHookContext::default())
            .await;
        // Mutations dropped; chain continues without abort.
        assert!(result.mutations.is_empty());
        assert!(!result.is_aborted());
    }

    /// SPEC-051 AC-1: a hook is only invoked for events listed in its
    /// `events()` slice.
    #[tokio::test]
    async fn hook_only_invoked_for_its_events() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mgr = SyncHookManager::new();
        mgr.register(Arc::new(CountingHook {
            counter: counter.clone(),
            accept: SyncHookEvent::PreLLMRequest,
        }))
        .await;
        // PreCompact: counter must NOT increment.
        let _ = mgr
            .dispatch(SyncHookEvent::PreCompact, &SyncHookContext::default())
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        // PreLLMRequest: counter increments once.
        let _ = mgr
            .dispatch(SyncHookEvent::PreLLMRequest, &SyncHookContext::default())
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
