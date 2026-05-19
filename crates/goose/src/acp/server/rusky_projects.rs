// ── RUSKY FORK PATCH: _rusky/projects/create ─────────────────────────────────
//
// Handler for the `_rusky/projects/create` ACP method.
//
// The renderer-side "+ New Project" sidebar CTA collects a display name
// from the user and POSTs it through this method. The handler:
//
//   1. Validates the name (1-64 chars, no path-traversal characters).
//   2. Resolves the parent directory — caller-supplied or
//      `~/Documents/Rusky Projects/`.
//   3. Creates a slugified project folder under the parent (with a
//      numeric suffix on collision).
//   4. Writes a `.rusky-project.json` marker file containing a stable
//      UUID + name + creation timestamp.
//   5. Returns `{ project_id, root_path }`.
//
// The wider project metadata (icon, color, working dirs, prompt) still
// flows through `_goose/sources/*` — this method is the minimal-friction
// happy path for the sidebar CTA. The renderer is expected to refresh
// its project list after a successful call.

use super::GooseAcpAgent;
use crate::acp::custom_requests::{CreateProjectRequest, CreateProjectResponse};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum number of bytes (UTF-8) allowed in the display name. Mirrors
/// the upper bound on `_goose/sources/create` so the two surfaces don't
/// disagree on what a "project name" is.
const NAME_MAX_BYTES: usize = 64;

/// Maximum slug-collision counter before the handler gives up and
/// surfaces an error. Pathological — exists to bound the loop.
const MAX_SLUG_SUFFIX: u32 = 1024;

/// Default parent directory when the caller omits `parent_dir`. Picked
/// so it sits alongside the user's other "real" project folders rather
/// than buried inside `~/Library/Application Support`.
const DEFAULT_PARENT_SUBDIR: &str = "Rusky Projects";

/// Marker file dropped at the root of every Rusky project. Identifies
/// the directory as a Rusky project to future scans (e.g. `_goose/
/// sources/import`).
const MARKER_FILE_NAME: &str = ".rusky-project.json";

/// On-disk contents of `MARKER_FILE_NAME`. Stable across renames of the
/// containing folder — the `project_id` is the canonical identifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMarker {
    /// UUID v4 generated at project creation.
    pub id: String,
    /// Display name as supplied by the user.
    pub name: String,
    /// ISO-8601 (RFC 3339) timestamp of creation.
    pub created_at: String,
}

impl GooseAcpAgent {
    /// SPEC: `_rusky/projects/create` — see module doc.
    pub async fn on_project_create(
        &self,
        req: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, agent_client_protocol::Error> {
        let name = validate_name(&req.name)?;
        let parent = resolve_parent_dir(req.parent_dir.as_deref())?;
        ensure_dir(&parent)?;

        let (project_id, root_path) = create_project_at(&parent, name)?;

        tracing::info!(
            target: "goose::acp::rusky_projects",
            event = "rusky_projects_acp_call",
            method = "create",
            outcome = "ok",
            project_id = %project_id,
            // root_path is logged at info because it is non-PII (path
            // already exists on the user's machine — they typed the name).
            root_path = %root_path.display(),
        );

        Ok(CreateProjectResponse {
            project_id,
            root_path: root_path.to_string_lossy().into_owned(),
        })
    }
}

// ─── Helpers (all `pub(super)` so the integration test crate can call
//      them directly without spinning up a `GooseAcpAgent`). ────────────

/// Validate the user-supplied display name. Returns the trimmed name
/// on success. Per SPEC: 1-64 UTF-8 bytes, no path-separator characters
/// (`/`, `\`), no parent-directory token (`..`), no NUL byte.
pub fn validate_name(raw: &str) -> Result<&str, agent_client_protocol::Error> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("project name must not be empty"));
    }
    if trimmed.len() > NAME_MAX_BYTES {
        return Err(agent_client_protocol::Error::invalid_params()
            .data(format!("project name exceeds {NAME_MAX_BYTES} bytes")));
    }
    // Reject the path-traversal vocabulary outright. We *could* sanitise
    // them away, but the contract is simpler if the caller never sends
    // them in the first place.
    if trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.contains('\0')
    {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("project name must not contain '/', '\\', '..', or NUL"));
    }
    Ok(trimmed)
}

/// Resolve `parent_dir` — caller-supplied or
/// `~/Documents/Rusky Projects/`. Does NOT create the directory.
pub fn resolve_parent_dir(
    explicit: Option<&str>,
) -> Result<PathBuf, agent_client_protocol::Error> {
    if let Some(p) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(p);
        if !path.is_absolute() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("parent_dir must be an absolute path"));
        }
        return Ok(path);
    }
    // `dirs::document_dir()` returns None on locked-down sandboxes; we
    // fall back to home so the call still succeeds. Tests can always
    // supply an explicit `parent_dir` to opt out of this path.
    let docs = dirs::document_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| {
            agent_client_protocol::Error::internal_error()
                .data("unable to resolve user document directory")
        })?;
    Ok(docs.join(DEFAULT_PARENT_SUBDIR))
}

/// Slug used for the on-disk folder. ASCII-lowercase, dashes between
/// runs of non-alphanumerics. Mirrors `uniqueProjectSlug` on the FE so
/// the two views of "what folder did my project get?" stay aligned.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "project".to_string()
    } else {
        out
    }
}

/// Create `parent` and any missing ancestors. Surfaced as
/// `internal_error` because a creation failure is a server-side issue.
fn ensure_dir(path: &Path) -> Result<(), agent_client_protocol::Error> {
    std::fs::create_dir_all(path).map_err(|e| {
        tracing::warn!(
            target: "goose::acp::rusky_projects",
            event = "rusky_projects_acp_call",
            method = "create",
            outcome = "mkdir_failed",
            error = %e,
        );
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to create parent directory: {e}"))
    })
}

/// Create the project folder + marker file. Returns `(project_id,
/// root_path)`. Caller has already validated `name`.
pub fn create_project_at(
    parent: &Path,
    name: &str,
) -> Result<(String, PathBuf), agent_client_protocol::Error> {
    let base_slug = slugify(name);
    let (root_path, _slug) = pick_unique_root(parent, &base_slug)?;

    std::fs::create_dir(&root_path).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to create project directory: {e}"))
    })?;

    let project_id = uuid::Uuid::new_v4().to_string();
    let marker = ProjectMarker {
        id: project_id.clone(),
        name: name.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let marker_path = root_path.join(MARKER_FILE_NAME);
    let marker_bytes = serde_json::to_vec_pretty(&marker).map_err(|e| {
        agent_client_protocol::Error::internal_error()
            .data(format!("failed to serialize project marker: {e}"))
    })?;
    if let Err(e) = std::fs::write(&marker_path, marker_bytes) {
        // Best-effort cleanup so a half-created directory does not
        // shadow a later retry with the same name.
        let _ = std::fs::remove_dir_all(&root_path);
        return Err(agent_client_protocol::Error::internal_error()
            .data(format!("failed to write project marker: {e}")));
    }

    Ok((project_id, root_path))
}

/// Pick a folder name under `parent` that doesn't yet exist. Tries
/// `<slug>`, `<slug>-2`, `<slug>-3`, … up to `MAX_SLUG_SUFFIX`.
fn pick_unique_root(
    parent: &Path,
    base: &str,
) -> Result<(PathBuf, String), agent_client_protocol::Error> {
    let primary = parent.join(base);
    if !primary.exists() {
        return Ok((primary, base.to_string()));
    }
    for i in 2..=MAX_SLUG_SUFFIX {
        let candidate_slug = format!("{base}-{i}");
        let candidate = parent.join(&candidate_slug);
        if !candidate.exists() {
            return Ok((candidate, candidate_slug));
        }
    }
    Err(agent_client_protocol::Error::internal_error()
        .data("could not pick a unique project folder name"))
}

// ── /RUSKY FORK PATCH: _rusky/projects/create ────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for `_rusky/projects/create`. Filesystem-touching
    //! happy-path and end-to-end roundtrips live in the
    //! `rusky_projects_acp` integration crate alongside the other
    //! `_rusky/*` integration suites.

    use super::*;

    #[test]
    fn validate_name_rejects_empty_and_whitespace() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert_eq!(validate_name("  Hello  ").unwrap(), "Hello");
    }

    #[test]
    fn validate_name_rejects_path_traversal() {
        for bad in [
            "../etc/passwd",
            "..",
            "foo/bar",
            "foo\\bar",
            "evil\0name",
        ] {
            assert!(validate_name(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn validate_name_rejects_oversize() {
        let long = "a".repeat(NAME_MAX_BYTES + 1);
        assert!(validate_name(&long).is_err());
        let ok = "a".repeat(NAME_MAX_BYTES);
        assert!(validate_name(&ok).is_ok());
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("MY APP!!"), "my-app");
        assert_eq!(slugify("  spaced  "), "spaced");
        // Fall back when no ASCII alphanumerics remain.
        assert_eq!(slugify("漢字"), "project");
        assert_eq!(slugify("---"), "project");
    }

    #[test]
    fn resolve_parent_dir_rejects_relative_path() {
        let err = resolve_parent_dir(Some("relative/path")).unwrap_err();
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v.get("code").and_then(|c| c.as_i64()), Some(-32602));
    }

    #[test]
    fn resolve_parent_dir_accepts_absolute_override() {
        let abs = if cfg!(windows) { "C:\\tmp\\rp" } else { "/tmp/rp" };
        let p = resolve_parent_dir(Some(abs)).unwrap();
        assert_eq!(p, PathBuf::from(abs));
    }
}
