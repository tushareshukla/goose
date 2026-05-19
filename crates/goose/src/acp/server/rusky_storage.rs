// ── RUSKY FORK PATCH: _rusky/storage/* + _rusky/sessions/{export,import} ─────
//
// Backend for the Settings → Storage pane (`rusky-app/src/features/settings/
// storage/`). Four ACP methods, all local-only:
//
//   _rusky/storage/sizes        — walks the cache/sessions/memory dirs and
//                                 reports byte totals for the disk-usage chart.
//   _rusky/storage/clear_cache  — `rm -rf` the platform cache dir under
//                                 "Project Rusky" (and equivalents on Linux/
//                                 Windows). Idempotent.
//   _rusky/sessions/export      — pack the goose sessions dir into a
//                                 `.tar.gz` under the platform temp dir and
//                                 return its absolute path so the desktop
//                                 shell can hand it to a save-file dialog.
//   _rusky/sessions/import      — extract a user-picked tarball back into the
//                                 sessions dir, rejecting zip-slip paths.
//
// Dispatched from `custom_dispatch.rs` (typed handlers) for the four request
// structs declared in `crates/goose-sdk/src/custom_requests.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder};

use super::GooseAcpAgent;
use crate::acp::custom_requests::{
    SessionsExportRequest, SessionsExportResponse, SessionsImportRequest,
    SessionsImportResponse, StorageClearCacheRequest, StorageClearCacheResponse,
    StorageSizesRequest, StorageSizesResponse,
};
use crate::config::paths::Paths;

const APP_DIR_NAME: &str = "Project Rusky";
const SESSIONS_FOLDER: &str = "sessions";

/// Return the platform cache directory used by Project Rusky.
///
/// - macOS:   ~/Library/Caches/Project Rusky
/// - Linux:   $XDG_CACHE_HOME/Project Rusky (or ~/.cache/Project Rusky)
/// - Windows: %LOCALAPPDATA%/Project Rusky/cache
fn rusky_cache_dir() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("RUSKY_CACHE_DIR") {
        return Some(PathBuf::from(override_path));
    }
    dirs::cache_dir().map(|base| base.join(APP_DIR_NAME))
}

/// Goose sessions directory — the same path `SessionManager::new` derives.
fn sessions_dir() -> PathBuf {
    Paths::data_dir().join(SESSIONS_FOLDER)
}

/// Compiled memory store. We point this at `memory/` under the data dir;
/// the actual proxy-backed store may live elsewhere on disk, but for the
/// usage display we report the local sidecar bytes.
fn memory_dir() -> PathBuf {
    Paths::data_dir().join("memory")
}

/// Recursive byte total of a directory tree. Missing dirs return 0 — the
/// caller does not need to special-case first-run installs.
fn dir_size_bytes(root: &Path) -> u64 {
    if !root.exists() {
        return 0;
    }
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Wipe the contents of `dir` but keep the directory itself in place so
/// subsequent writes don't fail. Returns bytes freed (best-effort: counted
/// before removal).
fn wipe_dir_contents(dir: &Path) -> std::io::Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut freed: u64 = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            freed = freed.saturating_add(dir_size_bytes(&path));
            // Ignore failures on individual children — best-effort wipe.
            let _ = fs::remove_dir_all(&path);
        } else {
            freed = freed.saturating_add(meta.len());
            let _ = fs::remove_file(&path);
        }
    }
    Ok(freed)
}

impl GooseAcpAgent {
    /// `_rusky/storage/sizes` — disk-usage breakdown for the Settings pane.
    pub(super) async fn on_storage_sizes(
        &self,
        _req: StorageSizesRequest,
    ) -> Result<StorageSizesResponse, agent_client_protocol::Error> {
        let cache_bytes = rusky_cache_dir()
            .map(|p| dir_size_bytes(&p))
            .unwrap_or(0);
        let sessions_bytes = dir_size_bytes(&sessions_dir());
        let memory_bytes = dir_size_bytes(&memory_dir());
        let total_bytes = cache_bytes
            .saturating_add(sessions_bytes)
            .saturating_add(memory_bytes);

        tracing::debug!(
            target: "goose::acp::rusky_storage",
            event = "rusky_storage_sizes",
            cache_bytes,
            sessions_bytes,
            memory_bytes,
            total_bytes,
        );
        Ok(StorageSizesResponse {
            cache_bytes,
            sessions_bytes,
            memory_bytes,
            total_bytes,
        })
    }

    /// `_rusky/storage/clear_cache` — wipe the Project Rusky cache dir.
    pub(super) async fn on_storage_clear_cache(
        &self,
        _req: StorageClearCacheRequest,
    ) -> Result<StorageClearCacheResponse, agent_client_protocol::Error> {
        let Some(cache) = rusky_cache_dir() else {
            return Ok(StorageClearCacheResponse { bytes_freed: 0 });
        };
        let freed = wipe_dir_contents(&cache).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("clear cache failed: {e}"))
        })?;
        tracing::info!(
            target: "goose::acp::rusky_storage",
            event = "rusky_storage_clear_cache",
            bytes_freed = freed,
            path = %cache.display(),
        );
        Ok(StorageClearCacheResponse { bytes_freed: freed })
    }

    /// `_rusky/sessions/export` — produce a tarball of the sessions dir.
    pub(super) async fn on_sessions_export(
        &self,
        req: SessionsExportRequest,
    ) -> Result<SessionsExportResponse, agent_client_protocol::Error> {
        let src = sessions_dir();
        let out_path = match req.target_path.as_deref() {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => {
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                std::env::temp_dir().join(format!("rusky-sessions-{stamp}.tar.gz"))
            }
        };
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    agent_client_protocol::Error::invalid_params()
                        .data(format!("create target parent: {e}"))
                })?;
            }
        }

        if !src.exists() {
            // Produce an empty tarball so the desktop save-file dialog
            // still has something concrete to hand the user.
            let file = fs::File::create(&out_path).map_err(|e| {
                agent_client_protocol::Error::internal_error()
                    .data(format!("create tarball: {e}"))
            })?;
            let enc = GzEncoder::new(file, Compression::default());
            let mut builder = Builder::new(enc);
            builder.finish().map_err(|e| {
                agent_client_protocol::Error::internal_error()
                    .data(format!("finalise tarball: {e}"))
            })?;
            return Ok(SessionsExportResponse {
                tarball_path: out_path.to_string_lossy().to_string(),
                bytes_written: 0,
            });
        }

        let bytes_written = dir_size_bytes(&src);

        let file = fs::File::create(&out_path).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("create tarball: {e}"))
        })?;
        let enc = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(enc);
        builder
            .append_dir_all(SESSIONS_FOLDER, &src)
            .map_err(|e| {
                agent_client_protocol::Error::internal_error()
                    .data(format!("pack tarball: {e}"))
            })?;
        builder.finish().map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("finalise tarball: {e}"))
        })?;

        tracing::info!(
            target: "goose::acp::rusky_storage",
            event = "rusky_sessions_export",
            tarball = %out_path.display(),
            bytes_written,
        );
        Ok(SessionsExportResponse {
            tarball_path: out_path.to_string_lossy().to_string(),
            bytes_written,
        })
    }

    /// `_rusky/sessions/import` — extract a user-picked tarball back into
    /// the sessions directory.
    pub(super) async fn on_sessions_import(
        &self,
        req: SessionsImportRequest,
    ) -> Result<SessionsImportResponse, agent_client_protocol::Error> {
        let src = PathBuf::from(&req.tarball_path);
        if !src.exists() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("tarball not found: {}", src.display())));
        }
        let dest = sessions_dir();
        fs::create_dir_all(&dest).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("prepare sessions dir: {e}"))
        })?;

        let file = fs::File::open(&src).map_err(|e| {
            agent_client_protocol::Error::invalid_params()
                .data(format!("open tarball: {e}"))
        })?;
        let dec = GzDecoder::new(file);
        let mut archive = Archive::new(dec);
        let entries = archive.entries().map_err(|e| {
            agent_client_protocol::Error::invalid_params()
                .data(format!("read tarball entries: {e}"))
        })?;

        let mut imported: u32 = 0;
        for entry in entries {
            let mut entry = entry.map_err(|e| {
                agent_client_protocol::Error::invalid_params()
                    .data(format!("read entry: {e}"))
            })?;
            let raw_path = entry.path().map_err(|e| {
                agent_client_protocol::Error::invalid_params()
                    .data(format!("entry path: {e}"))
            })?;
            // Strip the leading `sessions/` we wrote on export, so the
            // entries land directly under the dest sessions dir.
            let rel = raw_path
                .strip_prefix(SESSIONS_FOLDER)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| raw_path.to_path_buf());

            // Reject any entry whose normalized path tries to escape the
            // sessions dir (zip-slip defense).
            if rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                tracing::warn!(
                    target: "goose::acp::rusky_storage",
                    event = "rusky_sessions_import_skipped",
                    reason = "parent_dir_in_path",
                    path = %rel.display(),
                );
                continue;
            }
            let out_path = dest.join(&rel);
            // Ensure the resolved out_path stays under `dest`.
            if !out_path.starts_with(&dest) {
                tracing::warn!(
                    target: "goose::acp::rusky_storage",
                    event = "rusky_sessions_import_skipped",
                    reason = "escape_dest",
                    path = %rel.display(),
                );
                continue;
            }

            let is_dir = entry.header().entry_type().is_dir();
            if is_dir {
                let _ = fs::create_dir_all(&out_path);
                continue;
            }
            if let Some(parent) = out_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            entry.unpack(&out_path).map_err(|e| {
                agent_client_protocol::Error::internal_error()
                    .data(format!("unpack entry: {e}"))
            })?;
            imported = imported.saturating_add(1);
        }

        tracing::info!(
            target: "goose::acp::rusky_storage",
            event = "rusky_sessions_import",
            imported_count = imported,
            tarball = %src.display(),
        );
        Ok(SessionsImportResponse {
            imported_count: imported,
        })
    }
}

// ── Test-only helpers ────────────────────────────────────────────────────────
//
// The integration suite in `tests/rusky_storage_acp.rs` exercises the
// filesystem logic directly without spinning up a full `GooseAcpAgent`
// (that requires a provider factory + session manager). These free
// functions mirror the agent methods but take an explicit base dir so
// each test runs in isolation.

/// Compute sizes against an explicit base — used by integration tests.
pub fn sizes_under(
    cache_dir: &Path,
    sessions_dir: &Path,
    memory_dir: &Path,
) -> StorageSizesResponse {
    let cache_bytes = dir_size_bytes(cache_dir);
    let sessions_bytes = dir_size_bytes(sessions_dir);
    let memory_bytes = dir_size_bytes(memory_dir);
    StorageSizesResponse {
        cache_bytes,
        sessions_bytes,
        memory_bytes,
        total_bytes: cache_bytes
            .saturating_add(sessions_bytes)
            .saturating_add(memory_bytes),
    }
}

/// Clear an explicit cache dir — used by integration tests.
pub fn clear_cache_under(cache_dir: &Path) -> std::io::Result<u64> {
    wipe_dir_contents(cache_dir)
}

/// Pack `sessions_dir` into `out_tarball` and return bytes packed.
pub fn export_sessions_under(
    sessions_dir: &Path,
    out_tarball: &Path,
) -> std::io::Result<u64> {
    let bytes_written = dir_size_bytes(sessions_dir);
    let file = fs::File::create(out_tarball)?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(enc);
    if sessions_dir.exists() {
        builder.append_dir_all(SESSIONS_FOLDER, sessions_dir)?;
    }
    builder.finish()?;
    Ok(bytes_written)
}

/// Inflate `tarball` into `dest` — used by integration tests.
pub fn import_sessions_under(tarball: &Path, dest: &Path) -> std::io::Result<u32> {
    fs::create_dir_all(dest)?;
    let file = fs::File::open(tarball)?;
    let dec = GzDecoder::new(file);
    let mut archive = Archive::new(dec);
    let mut imported: u32 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?;
        let rel = raw_path
            .strip_prefix(SESSIONS_FOLDER)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| raw_path.to_path_buf());
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let out_path = dest.join(&rel);
        if !out_path.starts_with(dest) {
            continue;
        }
        if entry.header().entry_type().is_dir() {
            let _ = fs::create_dir_all(&out_path);
            continue;
        }
        if let Some(parent) = out_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        entry.unpack(&out_path)?;
        imported = imported.saturating_add(1);
    }
    Ok(imported)
}

// ── /RUSKY FORK PATCH ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn dir_size_walks_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("a/b");
        fs::create_dir_all(&sub).unwrap();
        let mut f = fs::File::create(sub.join("x.txt")).unwrap();
        f.write_all(b"hello world").unwrap();
        let mut g = fs::File::create(tmp.path().join("y.txt")).unwrap();
        g.write_all(b"hi").unwrap();
        assert_eq!(dir_size_bytes(tmp.path()), 13);
    }

    #[test]
    fn wipe_keeps_root_removes_children() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("x");
        fs::create_dir_all(&sub).unwrap();
        let mut f = fs::File::create(sub.join("a.txt")).unwrap();
        f.write_all(b"abc").unwrap();
        let freed = wipe_dir_contents(tmp.path()).unwrap();
        assert_eq!(freed, 3);
        assert!(tmp.path().exists());
        assert!(!sub.exists());
    }

    #[test]
    fn missing_cache_dir_returns_zero() {
        let missing = std::env::temp_dir().join("does-not-exist-rusky-acp-test");
        // ensure it actually doesn't exist
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(dir_size_bytes(&missing), 0);
    }
}
