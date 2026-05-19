//! Integration tests for the `_rusky/automations/*` ACP method family
//! (SPEC-061 § "API contract").
//!
//! The handlers in `crate::acp::server::rusky_automations` are filesystem-backed
//! (recipes, scheduler state, run history). Each test isolates state by
//! pointing `RUSKY_AUTOMATIONS_STATE_DIR` and `GOOSE_PATH_ROOT` at a temp
//! directory. We drive the dispatcher with JSON params and assert on the
//! wire shape the frontend depends on (rusky-app/.../automations/api).
//!
//! AC coverage map (SPEC-061):
//!   AC-1, AC-2  → ac1_ac2_list_returns_recipes_and_schedules
//!   AC-5        → ac5_run_now_returns_run_id_and_records_history
//!   AC-6, AC-12 → ac6_ac12_schedule_register_and_unregister_round_trip
//!   AC-9        → ac9_nl_to_recipe_extracts_5_field_cron
//!   AC-10       → ac10_validate_rejects_missing_title
//!   AC-11       → ac11_propose_writes_recipe_to_disk
//!   AC-22       → ac22_stop_marks_history_row_cancelled
//!   AC-4, OQ-4  → ac4_history_list_caps_at_default_100
//!   AC-13       → ac13_propose_idempotent_schedule_register

#![cfg(feature = "rusky-automations")]

use goose::acp::server::rusky_automations::dispatch_rusky_automations;
use serde_json::json;
use std::sync::Mutex;
use tempfile::TempDir;

// Tests in this file mutate process-wide env vars and the in-memory running
// registry. The lock serialises them so cargo's per-crate parallelism
// doesn't produce flakes.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _g: std::sync::MutexGuard<'static, ()>,
    _tmp: TempDir,
    prev_state_dir: Option<String>,
    prev_path_root: Option<String>,
}

impl EnvGuard {
    fn new() -> Self {
        let g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = TempDir::new().expect("tempdir");
        let prev_state_dir = std::env::var("RUSKY_AUTOMATIONS_STATE_DIR").ok();
        let prev_path_root = std::env::var("GOOSE_PATH_ROOT").ok();
        std::env::set_var("RUSKY_AUTOMATIONS_STATE_DIR", tmp.path());
        std::env::set_var("GOOSE_PATH_ROOT", tmp.path());
        Self {
            _g: g,
            _tmp: tmp,
            prev_state_dir,
            prev_path_root,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev_state_dir {
            Some(p) => std::env::set_var("RUSKY_AUTOMATIONS_STATE_DIR", p),
            None => std::env::remove_var("RUSKY_AUTOMATIONS_STATE_DIR"),
        }
        match &self.prev_path_root {
            Some(p) => std::env::set_var("GOOSE_PATH_ROOT", p),
            None => std::env::remove_var("GOOSE_PATH_ROOT"),
        }
    }
}

fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("RUSKY_AUTOMATIONS_STATE_DIR").unwrap())
}

async fn call(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    dispatch_rusky_automations(method, params)
        .await
        .unwrap_or_else(|| panic!("method {method} not handled by automations dispatcher"))
}

// SPEC-061 AC-1, AC-2.
#[tokio::test]
async fn ac1_ac2_list_returns_recipes_and_schedules() {
    let _g = EnvGuard::new();
    // GOOSE_PATH_ROOT redirects Paths::config_dir to ${tmp}/config, so the
    // recipe scan visits `${tmp}/config/recipes/`.
    let recipes_dir = state_dir().join("config").join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::write(
        recipes_dir.join("daily-briefing.yaml"),
        "version: \"1.0.0\"\ntitle: \"Daily briefing\"\ndescription: \"Briefing\"\ninstructions: |\n  Brief me.\n",
    )
    .unwrap();
    std::fs::write(
        recipes_dir.join("weekly-review.yaml"),
        "version: \"1.0.0\"\ntitle: \"Weekly review\"\ndescription: \"Review\"\ninstructions: |\n  Recap the week.\n",
    )
    .unwrap();

    let result = call("_rusky/automations/list", json!({})).await.unwrap();
    let recipes = result.get("recipes").and_then(|v| v.as_array()).unwrap();
    assert!(
        recipes.len() >= 2,
        "expected at least 2 recipes, got {}",
        recipes.len()
    );
    let schedules = result.get("schedules").and_then(|v| v.as_array()).unwrap();
    assert!(schedules.is_empty(), "no schedules registered yet");
}

// SPEC-061 AC-5.
#[tokio::test]
async fn ac5_run_now_returns_run_id_and_records_history() {
    let _g = EnvGuard::new();
    let result = call(
        "_rusky/automations/run_now",
        json!({ "recipeId": "daily-briefing" }),
    )
    .await
    .unwrap();
    let run_id = result
        .get("runId")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(run_id.starts_with("run_"));
    let hist = call(
        "_rusky/automations/history_list",
        json!({ "limit": 10, "offset": 0 }),
    )
    .await
    .unwrap();
    let runs = hist.get("runs").and_then(|v| v.as_array()).unwrap();
    assert!(runs.iter().any(|r| r["runId"].as_str() == Some(&run_id)));
}

// SPEC-061 AC-6, AC-12.
#[tokio::test]
async fn ac6_ac12_schedule_register_and_unregister_round_trip() {
    let _g = EnvGuard::new();
    let reg = call(
        "_rusky/automations/schedule_register",
        json!({ "recipeId": "daily-briefing", "cron": "0 8 * * 1-5" }),
    )
    .await
    .unwrap();
    assert_eq!(reg["ok"].as_bool(), Some(true));

    let listed = call("_rusky/automations/list", json!({})).await.unwrap();
    let schedules = listed["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0]["recipeId"].as_str(), Some("daily-briefing"));
    assert_eq!(schedules[0]["cron"].as_str(), Some("0 8 * * 1-5"));
    assert_eq!(schedules[0]["enabled"].as_bool(), Some(true));

    let _ = call(
        "_rusky/automations/schedule_unregister",
        json!({ "recipeId": "daily-briefing" }),
    )
    .await
    .unwrap();
    let listed2 = call("_rusky/automations/list", json!({})).await.unwrap();
    assert!(listed2["schedules"].as_array().unwrap().is_empty());
}

// SPEC-061 AC-9.
#[tokio::test]
async fn ac9_nl_to_recipe_extracts_5_field_cron() {
    let _g = EnvGuard::new();
    let value = call(
        "_rusky/automations/nl_to_recipe",
        json!({ "description": "every Friday at 5 p.m. draft a recap" }),
    )
    .await
    .unwrap();
    assert_eq!(value["cron"].as_str(), Some("0 17 * * 5"));
    let yaml = value["yaml"].as_str().unwrap();
    let v = call("_rusky/automations/validate", json!({ "yaml": yaml }))
        .await
        .unwrap();
    assert_eq!(v["ok"].as_bool(), Some(true));
}

// SPEC-061 AC-10.
#[tokio::test]
async fn ac10_validate_rejects_missing_title() {
    let _g = EnvGuard::new();
    let bad = "version: \"1.0.0\"\ndescription: \"no title\"\ninstructions: \"x\"\n";
    let v = call("_rusky/automations/validate", json!({ "yaml": bad }))
        .await
        .unwrap();
    assert_eq!(v["ok"].as_bool(), Some(false));
    assert!(v["errors"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false));
}

// SPEC-061 AC-11.
#[tokio::test]
async fn ac11_propose_writes_recipe_to_disk() {
    let _g = EnvGuard::new();
    let yaml = "version: \"1.0.0\"\ntitle: \"Photo organizer\"\ndescription: \"Sort photos\"\ninstructions: |\n  Sort the photos.\n";
    let v = call("_rusky/automations/propose", json!({ "yaml": yaml }))
        .await
        .unwrap();
    assert_eq!(v["recipeId"].as_str(), Some("photo-organizer"));
    let target = state_dir().join("recipes").join("photo-organizer.yaml");
    assert!(target.exists(), "recipe should be persisted to disk");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "recipe file must be 0600");
    }
}

// SPEC-061 AC-22.
#[tokio::test]
async fn ac22_stop_marks_history_row_cancelled() {
    let _g = EnvGuard::new();
    let run = call("_rusky/automations/run_now", json!({ "recipeId": "x" }))
        .await
        .unwrap();
    let run_id = run["runId"].as_str().unwrap().to_string();
    let stop = call("_rusky/automations/stop", json!({ "runId": run_id }))
        .await
        .unwrap();
    assert_eq!(stop["ok"].as_bool(), Some(true));
    let hist = call(
        "_rusky/automations/history_list",
        json!({ "limit": 10, "offset": 0 }),
    )
    .await
    .unwrap();
    let row = hist["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["runId"].as_str() == Some(&run_id))
        .expect("history row");
    assert_eq!(row["outcome"].as_str(), Some("cancelled"));
    assert_eq!(row["failureKind"].as_str(), Some("user_stopped"));
}

// SPEC-061 AC-4 + OQ-4.
#[tokio::test]
async fn ac4_history_list_caps_at_default_100() {
    let _g = EnvGuard::new();
    for _ in 0..150 {
        call("_rusky/automations/run_now", json!({ "recipeId": "x" }))
            .await
            .unwrap();
    }
    let hist0 = call(
        "_rusky/automations/history_list",
        json!({ "limit": 0, "offset": 0 }),
    )
    .await
    .unwrap();
    assert_eq!(hist0["runs"].as_array().unwrap().len(), 100);
}

// SPEC-061 AC-13.
#[tokio::test]
async fn ac13_propose_idempotent_schedule_register() {
    let _g = EnvGuard::new();
    let yaml = "version: \"1.0.0\"\ntitle: \"Daily briefing\"\ndescription: \"Brief\"\ninstructions: |\n  Brief me.\n";
    let _ = call(
        "_rusky/automations/propose",
        json!({ "yaml": yaml, "cron": "0 8 * * 1-5" }),
    )
    .await
    .unwrap();
    let _ = call(
        "_rusky/automations/propose",
        json!({ "yaml": yaml, "cron": "0 9 * * 1-5" }),
    )
    .await
    .unwrap();
    let listed = call("_rusky/automations/list", json!({})).await.unwrap();
    let schedules = listed["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1, "schedule must remain a single tuple");
    assert_eq!(schedules[0]["cron"].as_str(), Some("0 9 * * 1-5"));
}

#[tokio::test]
async fn invalid_cron_returns_invalid_params() {
    let _g = EnvGuard::new();
    let result = dispatch_rusky_automations(
        "_rusky/automations/schedule_register",
        json!({ "recipeId": "x", "cron": "not a cron" }),
    )
    .await
    .unwrap();
    let err = result.expect_err("invalid cron must err");
    let v = serde_json::to_value(&err).unwrap();
    // -32602 invalid params.
    assert_eq!(v["code"].as_i64(), Some(-32602));
}

#[tokio::test]
async fn empty_description_rejected_with_invalid_params() {
    let _g = EnvGuard::new();
    let result = dispatch_rusky_automations(
        "_rusky/automations/nl_to_recipe",
        json!({ "description": "   " }),
    )
    .await
    .unwrap();
    let err = result.expect_err("empty desc must err");
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(v["code"].as_i64(), Some(-32602));
}

#[tokio::test]
async fn unknown_method_falls_through() {
    let _g = EnvGuard::new();
    let r = dispatch_rusky_automations("_rusky/automations/does-not-exist", json!({})).await;
    assert!(r.is_none(), "unknown methods must return None");
}
