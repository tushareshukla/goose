// ── RUSKY FORK PATCH: _rusky/distro/info ─────────────────────────────────────
//
// Handler for the _rusky/distro/info ACP method.
//
// Reads the distro manifest using the following discovery order:
//   1. RUSKY_DISTRO_PATH environment variable
//   2. Platform bundle resource path (macOS: Rusky.app/Contents/Resources/distro/)
//   3. Built-in stub returning an empty-toggles/empty-allowlist default
//
// See SPEC-051, SPEC-012, ADR-005.

use super::GooseAcpAgent;
use crate::acp::custom_requests::{DistroInfoRequest, DistroInfoResponse};
use std::collections::HashMap;

impl GooseAcpAgent {
    pub async fn on_distro_info(
        &self,
        _req: DistroInfoRequest,
    ) -> Result<DistroInfoResponse, agent_client_protocol::Error> {
        let _span = tracing::info_span!(
            "rusky.distro_info",
            toggle_count = tracing::field::Empty,
            allowlist_count = tracing::field::Empty,
        )
        .entered();

        let response = load_distro_response();

        tracing::Span::current().record("toggle_count", response.feature_toggles.len());
        tracing::Span::current().record("allowlist_count", response.extension_allowlist.len());

        Ok(response)
    }
}

/// Load the distro manifest from the discovery chain:
/// 1. RUSKY_DISTRO_PATH env var
/// 2. macOS bundle resource path
/// 3. Built-in stub (safe default — all toggles false, empty allowlist)
fn load_distro_response() -> DistroInfoResponse {
    let locked_at = chrono::Utc::now().to_rfc3339();

    // Path 1: RUSKY_DISTRO_PATH environment variable
    if let Ok(path) = std::env::var("RUSKY_DISTRO_PATH") {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(resp) = serde_json::from_slice::<DistroInfoResponse>(&bytes) {
                tracing::info!(source = "env", path = %path, "distro loaded from RUSKY_DISTRO_PATH");
                return DistroInfoResponse { locked_at, ..resp };
            }
            tracing::warn!(
                source = "env",
                path = %path,
                "RUSKY_DISTRO_PATH set but JSON parse failed; falling through to bundle path"
            );
        }
    }

    // Path 2: macOS bundle resource path (Rusky.app/Contents/Resources/distro/distro.json)
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            // goosed lives at Rusky.app/Contents/MacOS/goosed
            // resource path:      Rusky.app/Contents/Resources/distro/distro.json
            if let Some(macos_dir) = exe.parent() {
                let bundle_path = macos_dir
                    .join("../Resources/distro/distro.json")
                    .canonicalize()
                    .unwrap_or_default();
                if bundle_path.exists() {
                    if let Ok(bytes) = std::fs::read(&bundle_path) {
                        if let Ok(resp) = serde_json::from_slice::<DistroInfoResponse>(&bytes) {
                            tracing::info!(
                                source = "bundle",
                                path = %bundle_path.display(),
                                "distro loaded from macOS bundle resources"
                            );
                            return DistroInfoResponse { locked_at, ..resp };
                        }
                    }
                }
            }
        }
    }

    // Path 3: Built-in stub — guarantees non-Rusky goose builds never panic.
    tracing::info!(
        source = "stub",
        "distro not found; returning built-in stub (all toggles false, empty allowlist)"
    );
    DistroInfoResponse {
        feature_toggles: HashMap::new(),
        extension_allowlist: Vec::new(),
        app_version: "stub".to_string(),
        locked_at,
    }
}

// ── /RUSKY FORK PATCH: _rusky/distro/info ────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distro_info_returns_stub() {
        // When no env var and no bundle, should get stub (empty maps, "stub" version)
        // Remove env var if set so this test is deterministic
        std::env::remove_var("RUSKY_DISTRO_PATH");

        let resp = load_distro_response();
        // Stub must not panic and must return valid response shape
        assert_eq!(resp.app_version, "stub");
        assert!(resp.feature_toggles.is_empty());
        assert!(resp.extension_allowlist.is_empty());
        assert!(!resp.locked_at.is_empty(), "locked_at must be a timestamp");
    }

    #[test]
    fn test_distro_info_env_path_valid_json() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let payload = serde_json::json!({
            "featureToggles": {
                "voice": true,
                "memory": false
            },
            "extensionAllowlist": ["developer", "browser"],
            "appVersion": "1.0.0-test",
            "lockedAt": "2026-05-17T00:00:00Z"
        });
        tmp.write_all(payload.to_string().as_bytes()).unwrap();
        tmp.flush().unwrap();

        let path = tmp.path().to_string_lossy().to_string();
        std::env::set_var("RUSKY_DISTRO_PATH", &path);

        let resp = load_distro_response();
        assert_eq!(resp.app_version, "1.0.0-test");
        assert_eq!(resp.feature_toggles.get("voice"), Some(&true));
        assert_eq!(resp.extension_allowlist.len(), 2);

        std::env::remove_var("RUSKY_DISTRO_PATH");
    }

    #[test]
    fn test_distro_info_env_path_invalid_json_falls_through() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"not-valid-json").unwrap();
        tmp.flush().unwrap();

        let path = tmp.path().to_string_lossy().to_string();
        std::env::set_var("RUSKY_DISTRO_PATH", &path);

        // Should not panic; falls through to stub
        let resp = load_distro_response();
        assert_eq!(resp.app_version, "stub");

        std::env::remove_var("RUSKY_DISTRO_PATH");
    }
}
