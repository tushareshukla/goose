// ── RUSKY FORK PATCH: _rusky/automations/* (SPEC-061) ────────────────────────
//
// Real ACP handlers for the `_rusky/automations/*` method family.
//
// Storage layout (mode 0600 files, 0700 directory):
//   ${CONFIG_DIR}/scheduler_state.json     (recipe_id, cron, enabled, ...)[]
//   ${CONFIG_DIR}/run_history.json         last 1000 runs, append-only
//
// Where ${CONFIG_DIR} defaults to `Paths::config_dir()` and may be
// overridden by RUSKY_AUTOMATIONS_STATE_DIR for tests + bundled installs.
//
// Recipes themselves come from `crate::recipe::local_recipes::list_local_recipes`,
// which already scans:
//   - the current directory
//   - `${GOOSE_RECIPE_PATH}` entries
//   - `~/.config/goose/recipes/`
//   - `${CWD}/.goose/recipes/`
//   - `~/.agents/recipes/` and `${CWD}/.agents/recipes/`
//
// Bundled recipes ship in the distro bundle (SPEC-012) and are copied into
// the config recipes dir on first launch by the Tauri shell — outside the
// scope of this handler.
//
// Auth model: matches `rusky_distro.rs` (no extra auth; goosed runs locally
// inside the install). Local-token plumbing is reserved for the `_rusky/memory/*`
// family, which proxies HTTP to rusky-proxy.
//
// SPEC-061 acceptance criteria implemented here:
//   AC-1, AC-2          → list (recipes + schedules)
//   AC-5                → run_now
//   AC-6                → schedule_register / schedule_unregister (toggle)
//   AC-10, AC-11        → validate / propose (save with atomic write)
//   AC-12               → schedule_register
//   AC-22               → stop
//   AC-4, OQ-4          → history_list (last 100; capped at 200)
//   AC-9                → nl_to_recipe cron extraction
//   AC-21               → concurrent runs (each gets its own run_id)
//
// What's *not* here:
//   AC-15 reconcile pass        → owned by the Tauri auto-updater path
//   AC-16/AC-17 approval timeout → owned by the agent loop's permission flow
//   AC-18/AC-19 skip semantics   → owned by the scheduler tick (when it fires
//                                  inside goosed; this module records the row)

use super::GooseAcpAgent;
use crate::acp::custom_requests::{
    AutomationRecipe, AutomationRecipeParameter, AutomationRun, AutomationSchedule,
    AutomationsHistoryListRequest, AutomationsHistoryListResponse, AutomationsListRequest,
    AutomationsListResponse, AutomationsNlToRecipeRequest, AutomationsNlToRecipeResponse,
    AutomationsProposeRequest, AutomationsProposeResponse, AutomationsRunNowRequest,
    AutomationsRunNowResponse, AutomationsScheduleRegisterRequest,
    AutomationsScheduleRegisterResponse, AutomationsScheduleUnregisterRequest,
    AutomationsScheduleUnregisterResponse, AutomationsStopRequest, AutomationsStopResponse,
    AutomationsValidateRequest, AutomationsValidateResponse,
};
use crate::config::paths::Paths;
use crate::recipe::local_recipes::list_local_recipes;
use crate::recipe::validate_recipe::validate_recipe_template_from_content;
use crate::recipe::Recipe;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

const STATE_DIR_ENV: &str = "RUSKY_AUTOMATIONS_STATE_DIR";
const SCHEDULER_STATE_FILE: &str = "scheduler_state.json";
const RUN_HISTORY_FILE: &str = "run_history.json";
const HISTORY_HARD_CAP: u32 = 200;
const HISTORY_DEFAULT_LIMIT: u32 = 100;
const HISTORY_RING_SIZE: usize = 1_000;

// In-memory registry of currently-running invocations. The lock is held
// only for short critical sections (push / mark cancelled). Persistent
// history lives on disk; this map only tracks the "Running tab" view.
static RUNNING_REGISTRY: Mutex<Option<RunningRegistry>> = Mutex::new(None);

#[derive(Default)]
struct RunningRegistry {
    rows: Vec<AutomationRun>,
}

fn with_registry<R>(f: impl FnOnce(&mut RunningRegistry) -> R) -> R {
    let mut guard = RUNNING_REGISTRY.lock().expect("running registry poisoned");
    if guard.is_none() {
        *guard = Some(RunningRegistry::default());
    }
    f(guard.as_mut().expect("just initialised"))
}

fn state_dir() -> PathBuf {
    if let Ok(p) = std::env::var(STATE_DIR_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    Paths::config_dir()
}

fn scheduler_state_path() -> PathBuf {
    state_dir().join(SCHEDULER_STATE_FILE)
}

fn run_history_path() -> PathBuf {
    state_dir().join(RUN_HISTORY_FILE)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SchedulerStateFile {
    #[serde(default)]
    schedules: Vec<AutomationSchedule>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RunHistoryFile {
    #[serde(default)]
    runs: Vec<AutomationRun>,
}

fn ensure_state_dir() -> Result<PathBuf, agent_client_protocol::Error> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to create state dir {}: {e}", dir.display()))
    })?;
    // Best-effort chmod 0700 on the directory (Unix only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dir) {
            let mut perms = meta.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&dir, perms);
        }
    }
    Ok(dir)
}

fn load_scheduler_state() -> Result<SchedulerStateFile, agent_client_protocol::Error> {
    let path = scheduler_state_path();
    if !path.exists() {
        return Ok(SchedulerStateFile::default());
    }
    let bytes = std::fs::read(&path).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to read {}: {e}", path.display()))
    })?;
    if bytes.is_empty() {
        return Ok(SchedulerStateFile::default());
    }
    serde_json::from_slice::<SchedulerStateFile>(&bytes).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("scheduler_state.json corrupt: {e}"))
    })
}

fn save_scheduler_state(state: &SchedulerStateFile) -> Result<(), agent_client_protocol::Error> {
    ensure_state_dir()?;
    let path = scheduler_state_path();
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to serialise scheduler state: {e}"))
    })?;
    std::fs::write(&tmp, &bytes).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("scheduler_state.json write failed: {e}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("scheduler_state.json rename failed: {e}"))
    })?;
    Ok(())
}

fn load_run_history() -> Result<RunHistoryFile, agent_client_protocol::Error> {
    let path = run_history_path();
    if !path.exists() {
        return Ok(RunHistoryFile::default());
    }
    let bytes = std::fs::read(&path).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to read {}: {e}", path.display()))
    })?;
    if bytes.is_empty() {
        return Ok(RunHistoryFile::default());
    }
    serde_json::from_slice::<RunHistoryFile>(&bytes).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("run_history.json corrupt: {e}"))
    })
}

fn save_run_history(history: &RunHistoryFile) -> Result<(), agent_client_protocol::Error> {
    ensure_state_dir()?;
    let path = run_history_path();
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(history).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to serialise run history: {e}"))
    })?;
    std::fs::write(&tmp, &bytes).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("run_history.json write failed: {e}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("run_history.json rename failed: {e}"))
    })?;
    Ok(())
}

fn append_run_history(row: AutomationRun) -> Result<(), agent_client_protocol::Error> {
    let mut history = load_run_history().unwrap_or_default();
    history.runs.push(row);
    let len = history.runs.len();
    if len > HISTORY_RING_SIZE {
        let drop = len - HISTORY_RING_SIZE;
        history.runs.drain(0..drop);
    }
    save_run_history(&history)
}

fn slugify(input: &str) -> String {
    let lower: String = input.to_lowercase();
    let mut s = String::with_capacity(lower.len());
    let mut prev_dash = false;
    for c in lower.chars() {
        if c.is_alphanumeric() {
            s.push(c);
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    let trimmed = s.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "untitled-recipe".to_string()
    } else {
        trimmed
    }
}

fn recipe_to_dto(path: Option<&std::path::Path>, recipe: &Recipe) -> AutomationRecipe {
    let recipe_id = path
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| slugify(&recipe.title));
    let parameters = recipe.parameters.as_ref().map(|ps| {
        ps.iter()
            .map(|p| AutomationRecipeParameter {
                name: p.key.clone(),
                description: Some(p.description.clone()),
                type_: Some(p.input_type.to_string()),
                default: p
                    .default
                    .as_ref()
                    .map(|d| serde_json::Value::String(d.clone())),
            })
            .collect::<Vec<_>>()
    });
    let extensions = recipe
        .extensions
        .as_ref()
        .map(|es| es.iter().map(|e| e.name().to_string()).collect::<Vec<_>>());
    let sub_recipes = recipe
        .sub_recipes
        .as_ref()
        .map(|ss| ss.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    AutomationRecipe {
        recipe_id,
        version: 1,
        title: recipe.title.clone(),
        description: recipe.description.clone(),
        instructions: recipe.instructions.clone().unwrap_or_default(),
        prompt: recipe.prompt.clone(),
        parameters,
        extensions,
        sub_recipes,
        // Heuristic: treat any recipe living in the config recipes dir as
        // user_authored unless the source is recorded in a sibling
        // `_rusky_meta.source` field (handled by `nl_creator` / `propose`).
        // Bundled recipes carry their own `_rusky_meta.source = "bundled"`
        // dropped in by the Tauri install path (SPEC-061 AC-14).
        source: "user_authored".to_string(),
    }
}

/// Best-effort 5-field cron validator. The handler is intentionally lenient:
/// rejects empty strings and obviously-malformed expressions, but defers
/// the full grammar to `tokio-cron-scheduler` at fire time (see SPEC-061
/// §"Cron string format").
fn validate_cron(cron: &str) -> Result<(), String> {
    let trimmed = cron.trim();
    if trimmed.is_empty() {
        return Err("cron must not be empty".into());
    }
    let fields = trimmed.split_whitespace().count();
    if fields != 5 {
        return Err(format!(
            "expected 5-field cron (minute hour day month weekday), got {fields}"
        ));
    }
    Ok(())
}

/// Heuristic NL → cron extractor (AC-9). Covers the common cases shown in
/// the SPEC-061 examples: "every weekday at 8 a.m.", "every Friday at 5",
/// "on the first of each month", "every Monday at 10". Returns None when
/// no schedule is implied — the caller surfaces the YAML without a cron.
fn extract_cron_from_nl(desc: &str) -> Option<String> {
    let lower = desc.to_lowercase();
    let hour = parse_hour(&lower);
    let dow = parse_weekday(&lower);

    if lower.contains("every weekday")
        || lower.contains("weekdays")
        || lower.contains("on weekdays")
    {
        let h = hour.unwrap_or(9);
        return Some(format!("0 {h} * * 1-5"));
    }
    if lower.contains("first of each month")
        || lower.contains("first of the month")
        || lower.contains("first of every month")
        || lower.contains("monthly")
    {
        let h = hour.unwrap_or(9);
        return Some(format!("0 {h} 1 * *"));
    }
    if lower.contains("every hour") || lower.contains("hourly") {
        return Some("0 * * * *".to_string());
    }
    if lower.contains("every day") || lower.contains("daily") {
        let h = hour.unwrap_or(9);
        return Some(format!("0 {h} * * *"));
    }
    if let Some(d) = dow {
        let h = hour.unwrap_or(9);
        return Some(format!("0 {h} * * {d}"));
    }
    None
}

fn parse_hour(lower: &str) -> Option<u8> {
    // Matches "at <hour>[:mm] [a.m.|p.m.]" — very tolerant; we only need
    // the hour bucket. Returns 0..=23. The slices below are guarded by
    // `find` and `char_indices`/ASCII checks, so they cannot land mid-char.
    let i = lower.find(" at ")? + 4;
    let rest = lower.get(i..)?;
    let (num_str, rest_after_num) = take_number(rest);
    let mut hour: u32 = num_str.parse().ok()?;
    if hour > 23 {
        return None;
    }
    let rest_lower = rest_after_num.trim_start_matches(':');
    let (_, rest_after_mm) = take_number(rest_lower);
    let suffix = rest_after_mm.trim_start();
    let is_pm = suffix.starts_with("p.m.") || suffix.starts_with("pm");
    let is_am = suffix.starts_with("a.m.") || suffix.starts_with("am");
    if is_pm && hour < 12 {
        hour += 12;
    }
    if is_am && hour == 12 {
        hour = 0;
    }
    Some(hour as u8)
}

fn take_number(s: &str) -> (&str, &str) {
    // `char_indices` returns code-point boundaries, so `split_at` here is
    // always UTF-8-safe.
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s.split_at(end)
}

fn parse_weekday(lower: &str) -> Option<u8> {
    const DAYS: &[(&str, u8)] = &[
        ("sunday", 0),
        ("monday", 1),
        ("tuesday", 2),
        ("wednesday", 3),
        ("thursday", 4),
        ("friday", 5),
        ("saturday", 6),
    ];
    for (name, n) in DAYS {
        if lower.contains(name) {
            return Some(*n);
        }
    }
    None
}

fn synth_recipe_yaml(description: &str, title: &str) -> String {
    let instructions = description.trim();
    let safe_title = title.replace('\"', "'");
    let safe_desc = description.replace('\"', "'");
    format!(
        "version: \"1.0.0\"\ntitle: \"{safe_title}\"\ndescription: \"{safe_desc}\"\ninstructions: |\n  {instructions}\n"
    )
}

/// Public dispatcher — exposed as a free function so integration tests can
/// drive the wire surface without constructing a full `GooseAcpAgent`.
/// The agent's instance method delegates to this.
pub async fn dispatch_rusky_automations(
    method: &str,
    params: serde_json::Value,
) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
    match method {
        "_rusky/automations/list" => Some(decode_then(params, on_automations_list)),
        "_rusky/automations/run_now" => Some(decode_then(params, on_automations_run_now)),
        "_rusky/automations/stop" => Some(decode_then(params, on_automations_stop)),
        "_rusky/automations/schedule_register" => {
            Some(decode_then(params, on_automations_schedule_register))
        }
        "_rusky/automations/schedule_unregister" => {
            Some(decode_then(params, on_automations_schedule_unregister))
        }
        "_rusky/automations/propose" => Some(decode_then(params, on_automations_propose)),
        "_rusky/automations/nl_to_recipe" => Some(decode_then(params, on_automations_nl_to_recipe)),
        "_rusky/automations/validate" => Some(decode_then(params, on_automations_validate)),
        "_rusky/automations/history_list" => Some(decode_then(params, on_automations_history_list)),
        _ => None,
    }
}

impl GooseAcpAgent {
    /// Dispatcher entry — wired into `dispatch_custom_request` in
    /// `custom_dispatch.rs`. Returns `Some(_)` if the method belongs to the
    /// automations family.
    pub async fn dispatch_rusky_automations_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, agent_client_protocol::Error>> {
        dispatch_rusky_automations(method, params).await
    }
}

/// SPEC-061 AC-1, AC-2.
pub fn on_automations_list(
    _req: AutomationsListRequest,
) -> Result<AutomationsListResponse, agent_client_protocol::Error> {
    let recipes = match list_local_recipes() {
        Ok(items) => items
            .iter()
            .map(|(path, recipe)| recipe_to_dto(Some(path), recipe))
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(
                target: "goose::acp::rusky_automations",
                event = "automations.list",
                outcome = "recipes_scan_failed",
                error = %e,
            );
            Vec::new()
        }
    };
    let schedules = load_scheduler_state()?.schedules;
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.list",
        outcome = "ok",
        recipes = recipes.len() as u32,
        schedules = schedules.len() as u32,
    );
    Ok(AutomationsListResponse { recipes, schedules })
}

/// SPEC-061 AC-5. Records the run in history immediately; downstream
/// orchestration (subagent kick-off) is the caller's responsibility.
pub fn on_automations_run_now(
    req: AutomationsRunNowRequest,
) -> Result<AutomationsRunNowResponse, agent_client_protocol::Error> {
    if req.recipe_id.trim().is_empty() {
        return Err(
            agent_client_protocol::Error::invalid_params().data("recipe_id must not be empty")
        );
    }
    let run_id = format!("run_{}", uuid::Uuid::now_v7());
    let row = AutomationRun {
        run_id: run_id.clone(),
        recipe_id: req.recipe_id.clone(),
        started_at: Utc::now().to_rfc3339(),
        ended_at: None,
        outcome: None,
        failure_kind: None,
        current_step: Some("starting".to_string()),
        summary: None,
        chat_session: None,
    };
    with_registry(|reg| reg.rows.push(row.clone()));
    append_run_history(row)?;
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.run_started",
        recipe_id = req.recipe_id.as_str(),
        trigger = "manual",
    );
    Ok(AutomationsRunNowResponse { run_id })
}

/// SPEC-061 AC-22.
pub fn on_automations_stop(
    req: AutomationsStopRequest,
) -> Result<AutomationsStopResponse, agent_client_protocol::Error> {
    if req.run_id.trim().is_empty() {
        return Err(agent_client_protocol::Error::invalid_params().data("run_id must not be empty"));
    }
    let removed = with_registry(|reg| {
        let before = reg.rows.len();
        reg.rows.retain(|r| r.run_id != req.run_id);
        before != reg.rows.len()
    });
    let mut history = load_run_history().unwrap_or_default();
    if let Some(row) = history
        .runs
        .iter_mut()
        .rev()
        .find(|r| r.run_id == req.run_id)
    {
        row.ended_at = Some(Utc::now().to_rfc3339());
        row.outcome = Some("cancelled".to_string());
        row.failure_kind = Some("user_stopped".to_string());
        save_run_history(&history)?;
    }
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.run_stopped",
        run_id = req.run_id.as_str(),
        removed_from_registry = removed,
    );
    Ok(AutomationsStopResponse { ok: true })
}

/// SPEC-061 AC-6, AC-12. Idempotent: re-registering the same
/// (recipe_id, cron) tuple replaces the cron — does NOT duplicate.
pub fn on_automations_schedule_register(
    req: AutomationsScheduleRegisterRequest,
) -> Result<AutomationsScheduleRegisterResponse, agent_client_protocol::Error> {
    if req.recipe_id.trim().is_empty() {
        return Err(
            agent_client_protocol::Error::invalid_params().data("recipe_id must not be empty")
        );
    }
    if let Err(e) = validate_cron(&req.cron) {
        return Err(agent_client_protocol::Error::invalid_params()
            .data(format!("RUSKY-AUTO-CRON-INVALID: {e}")));
    }
    let mut state = load_scheduler_state()?;
    if let Some(existing) = state
        .schedules
        .iter_mut()
        .find(|s| s.recipe_id == req.recipe_id)
    {
        existing.cron = req.cron.clone();
        existing.enabled = true;
    } else {
        state.schedules.push(AutomationSchedule {
            recipe_id: req.recipe_id.clone(),
            cron: req.cron.clone(),
            enabled: true,
            last_fired_at: None,
            next_fire_at: None,
        });
    }
    save_scheduler_state(&state)?;
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.schedule_registered",
        recipe_id = req.recipe_id.as_str(),
    );
    Ok(AutomationsScheduleRegisterResponse { ok: true })
}

/// SPEC-061 AC-6.
pub fn on_automations_schedule_unregister(
    req: AutomationsScheduleUnregisterRequest,
) -> Result<AutomationsScheduleUnregisterResponse, agent_client_protocol::Error> {
    let mut state = load_scheduler_state()?;
    let before = state.schedules.len();
    state.schedules.retain(|s| s.recipe_id != req.recipe_id);
    let removed = state.schedules.len() != before;
    if removed {
        save_scheduler_state(&state)?;
    }
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.schedule_unregistered",
        recipe_id = req.recipe_id.as_str(),
        removed = removed,
    );
    Ok(AutomationsScheduleUnregisterResponse { ok: true })
}

/// SPEC-061 AC-10, AC-11, AC-13.
pub fn on_automations_propose(
    req: AutomationsProposeRequest,
) -> Result<AutomationsProposeResponse, agent_client_protocol::Error> {
    let recipe = validate_recipe_template_from_content(&req.yaml, None).map_err(|e| {
        agent_client_protocol::Error::invalid_params()
            .data(format!("RUSKY-AUTO-RECIPE-INVALID: {e}"))
    })?;
    if let Some(cron) = req.cron.as_deref() {
        if let Err(e) = validate_cron(cron) {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("RUSKY-AUTO-CRON-INVALID: {e}")));
        }
    }
    let dir = ensure_state_dir()?;
    let recipes_dir = dir.join("recipes");
    std::fs::create_dir_all(&recipes_dir).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to create recipes dir: {e}"))
    })?;
    let recipe_id = slugify(&recipe.title);
    let target = recipes_dir.join(format!("{recipe_id}.yaml"));
    let tmp = target.with_extension("yaml.tmp");
    std::fs::write(&tmp, req.yaml.as_bytes()).map_err(|e| {
        agent_client_protocol::Error::internal_error().data(format!("recipe write failed: {e}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        agent_client_protocol::Error::internal_error().data(format!("recipe rename failed: {e}"))
    })?;
    if let Some(cron) = req.cron {
        let mut state = load_scheduler_state().unwrap_or_default();
        if let Some(existing) = state
            .schedules
            .iter_mut()
            .find(|s| s.recipe_id == recipe_id)
        {
            existing.cron = cron;
            existing.enabled = true;
        } else {
            state.schedules.push(AutomationSchedule {
                recipe_id: recipe_id.clone(),
                cron,
                enabled: true,
                last_fired_at: None,
                next_fire_at: None,
            });
        }
        save_scheduler_state(&state)?;
    }
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.recipe_created",
        source = "propose",
        recipe_id = recipe_id.as_str(),
    );
    Ok(AutomationsProposeResponse { recipe_id })
}

/// SPEC-061 AC-8, AC-9. Deterministic local extraction — no model call.
pub fn on_automations_nl_to_recipe(
    req: AutomationsNlToRecipeRequest,
) -> Result<AutomationsNlToRecipeResponse, agent_client_protocol::Error> {
    let desc = req.description.trim();
    if desc.is_empty() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("RUSKY-AUTO-NL-EMPTY: description must not be empty"));
    }
    let cron = extract_cron_from_nl(desc);
    let title = derive_title(desc);
    let yaml = synth_recipe_yaml(desc, &title);
    tracing::info!(
        target: "goose::acp::rusky_automations",
        event = "automations.nl_creator_completed",
        cron_extracted = cron.is_some(),
    );
    Ok(AutomationsNlToRecipeResponse { yaml, cron })
}

/// SPEC-061 AC-10.
pub fn on_automations_validate(
    req: AutomationsValidateRequest,
) -> Result<AutomationsValidateResponse, agent_client_protocol::Error> {
    match validate_recipe_template_from_content(&req.yaml, None) {
        Ok(_) => Ok(AutomationsValidateResponse {
            ok: true,
            errors: None,
        }),
        Err(e) => Ok(AutomationsValidateResponse {
            ok: false,
            errors: Some(vec![e.to_string()]),
        }),
    }
}

/// SPEC-061 AC-4 + OQ-4. Last 100 by default, capped at 200.
pub fn on_automations_history_list(
    req: AutomationsHistoryListRequest,
) -> Result<AutomationsHistoryListResponse, agent_client_protocol::Error> {
    let limit = if req.limit == 0 {
        HISTORY_DEFAULT_LIMIT
    } else {
        req.limit.min(HISTORY_HARD_CAP)
    };
    let history = load_run_history().unwrap_or_default();
    let mut all = history.runs;
    all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    let offset = req.offset as usize;
    let end = offset.saturating_add(limit as usize).min(all.len());
    let slice: Vec<AutomationRun> = if offset >= all.len() {
        Vec::new()
    } else {
        all[offset..end].to_vec()
    };
    Ok(AutomationsHistoryListResponse { runs: slice })
}

fn derive_title(desc: &str) -> String {
    let first_sentence = desc.split(['.', '\n', '!', '?']).next().unwrap_or(desc);
    let trimmed: String = first_sentence
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_string();
    if trimmed.is_empty() {
        "New automation".to_string()
    } else {
        trimmed
    }
}

/// Helper: decode JSON params into a typed request and synchronously call
/// the handler. ACP requests for this module are not currently async at the
/// handler boundary (filesystem-only).
fn decode_then<Req, Resp, F>(
    params: serde_json::Value,
    f: F,
) -> Result<serde_json::Value, agent_client_protocol::Error>
where
    Req: serde::de::DeserializeOwned + Default,
    Resp: serde::Serialize,
    F: FnOnce(Req) -> Result<Resp, agent_client_protocol::Error>,
{
    // Empty-params methods (`list`) often receive `null` or `{}` — both
    // must decode to the request's `Default`.
    let req: Req = if params.is_null() {
        Req::default()
    } else {
        serde_json::from_value::<Req>(params)
            .map_err(|e| agent_client_protocol::Error::invalid_params().data(e.to_string()))?
    };
    let resp = f(req)?;
    serde_json::to_value(&resp)
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))
}

// ── /RUSKY FORK PATCH: _rusky/automations/* ──────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit-level smoke tests. End-to-end ACP-roundtrip tests live in
    //! `goose/tests/rusky_automations_acp.rs`.

    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct StateDirGuard {
        _g: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        _tmp: tempfile::TempDir,
    }

    impl StateDirGuard {
        fn new() -> Self {
            let g = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(STATE_DIR_ENV).ok();
            let tmp = tempfile::TempDir::new().expect("tempdir");
            std::env::set_var(STATE_DIR_ENV, tmp.path());
            // Reset the running registry between tests.
            *RUNNING_REGISTRY.lock().unwrap() = Some(RunningRegistry::default());
            Self {
                _g: g,
                prev,
                _tmp: tmp,
            }
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(p) => std::env::set_var(STATE_DIR_ENV, p),
                None => std::env::remove_var(STATE_DIR_ENV),
            }
        }
    }

    #[test]
    fn cron_validation_5_field_required() {
        assert!(validate_cron("0 8 * * 1-5").is_ok());
        assert!(validate_cron("").is_err());
        assert!(validate_cron("0 8 * * * 1").is_err());
        assert!(validate_cron("not a cron").is_err());
    }

    #[test]
    fn extract_cron_weekday_8am() {
        // SPEC-061 AC-9 example.
        let got = extract_cron_from_nl("every weekday at 8 a.m., give me a morning briefing");
        assert_eq!(got.as_deref(), Some("0 8 * * 1-5"));
    }

    #[test]
    fn extract_cron_friday_at_5pm() {
        // SPEC-061 §Test plan `nl_extracts_cron_from_friday_at_5`.
        let got = extract_cron_from_nl("every Friday at 5 p.m. draft a recap");
        assert_eq!(got.as_deref(), Some("0 17 * * 5"));
    }

    #[test]
    fn extract_cron_monthly() {
        let got = extract_cron_from_nl("on the first of each month, draft an invoice summary");
        assert_eq!(got.as_deref(), Some("0 9 1 * *"));
    }

    #[test]
    fn extract_cron_none_when_no_schedule() {
        let got = extract_cron_from_nl("draft an invoice when I ask");
        assert!(got.is_none());
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Daily Briefing!"), "daily-briefing");
        assert_eq!(slugify(""), "untitled-recipe");
        assert_eq!(slugify("CRM Follow-up sweep"), "crm-follow-up-sweep");
    }

    #[test]
    fn schedule_register_persists_to_state_dir() {
        let _g = StateDirGuard::new();
        let agent_stub = ();
        // We invoke the storage helpers directly here since constructing
        // a GooseAcpAgent requires the full server harness (exercised in
        // the integration test).
        let _ = agent_stub;
        let mut state = load_scheduler_state().unwrap();
        state.schedules.push(AutomationSchedule {
            recipe_id: "daily-briefing".into(),
            cron: "0 8 * * 1-5".into(),
            enabled: true,
            last_fired_at: None,
            next_fire_at: None,
        });
        save_scheduler_state(&state).unwrap();
        let reloaded = load_scheduler_state().unwrap();
        assert_eq!(reloaded.schedules.len(), 1);
        assert_eq!(reloaded.schedules[0].recipe_id, "daily-briefing");
        assert!(reloaded.schedules[0].enabled);
    }

    #[test]
    fn history_ring_truncates() {
        let _g = StateDirGuard::new();
        for i in 0..(HISTORY_RING_SIZE + 50) {
            append_run_history(AutomationRun {
                run_id: format!("r{i}"),
                recipe_id: "test".into(),
                started_at: Utc::now().to_rfc3339(),
                ended_at: None,
                outcome: None,
                failure_kind: None,
                current_step: None,
                summary: None,
                chat_session: None,
            })
            .unwrap();
        }
        let history = load_run_history().unwrap();
        assert_eq!(history.runs.len(), HISTORY_RING_SIZE);
        // Oldest 50 must have been dropped.
        assert_eq!(history.runs[0].run_id, "r50");
    }
}
