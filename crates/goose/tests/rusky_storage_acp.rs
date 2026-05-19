//! Integration tests for the `_rusky/storage/*` + `_rusky/sessions/{export,
//! import}` ACP method family.
//!
//! These exercise the free-function helpers exported by
//! `goose::acp::server::rusky_storage` (`sizes_under`, `clear_cache_under`,
//! `export_sessions_under`, `import_sessions_under`). The full
//! `GooseAcpAgent` constructor is not reachable from a unit-test crate
//! without a provider factory + session manager + on-disk config; the
//! free-function entry points mirror the agent methods byte-for-byte and
//! let us exercise the same filesystem logic with an explicit base dir
//! per test (no env-var racing).
//!
//! AC map (Settings → Storage feature):
//!   AC-1 sizes/total            → sizes_total_is_sum_of_buckets
//!   AC-1 sizes/missing-dirs     → sizes_handles_missing_directories
//!   AC-2 clear_cache/wipes      → clear_cache_wipes_dir_contents
//!   AC-2 clear_cache/idempotent → clear_cache_on_empty_dir_returns_zero
//!   AC-3 export/round-trip      → export_then_import_recovers_files
//!   AC-3 export/empty-dir       → export_handles_empty_sessions_dir
//!   AC-4 import/zip-slip        → import_rejects_zip_slip_paths
//!   AC-4 import/missing-file    → import_fails_on_missing_tarball

use goose::acp::server::rusky_storage::{
    clear_cache_under, export_sessions_under, import_sessions_under, sizes_under,
};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Spin up a per-test workspace under `target/test-tmp` so failures leave
/// artefacts on disk for inspection while still being unique per test.
fn workspace(test_name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("rusky-storage-{test_name}-"))
        .tempdir()
        .expect("create temp workspace")
}

fn seed_file(dir: &std::path::Path, name: &str, contents: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("mkdir parent");
    let path = dir.join(name);
    let mut f = fs::File::create(&path).expect("create seeded file");
    f.write_all(contents).expect("write seeded file");
    path
}

// ─── AC-1: sizes ─────────────────────────────────────────────────────────────

#[test]
fn sizes_total_is_sum_of_buckets() {
    let ws = workspace("sizes-sum");
    let cache = ws.path().join("cache");
    let sessions = ws.path().join("sessions");
    let memory = ws.path().join("memory");
    seed_file(&cache, "c.bin", &vec![0u8; 100]);
    seed_file(&sessions, "s.bin", &vec![0u8; 250]);
    seed_file(&memory, "m.bin", &vec![0u8; 50]);

    let r = sizes_under(&cache, &sessions, &memory);
    assert_eq!(r.cache_bytes, 100);
    assert_eq!(r.sessions_bytes, 250);
    assert_eq!(r.memory_bytes, 50);
    assert_eq!(r.total_bytes, 400);
}

#[test]
fn sizes_handles_missing_directories() {
    let ws = workspace("sizes-missing");
    let cache = ws.path().join("cache-missing");
    let sessions = ws.path().join("sessions-missing");
    let memory = ws.path().join("memory-missing");
    // None of the paths exist — sizes_under must return zeros, not panic.
    let r = sizes_under(&cache, &sessions, &memory);
    assert_eq!(r.cache_bytes, 0);
    assert_eq!(r.sessions_bytes, 0);
    assert_eq!(r.memory_bytes, 0);
    assert_eq!(r.total_bytes, 0);
}

// ─── AC-2: clear_cache ───────────────────────────────────────────────────────

#[test]
fn clear_cache_wipes_dir_contents() {
    let ws = workspace("clear-wipes");
    let cache = ws.path().join("cache");
    let nested = cache.join("nested");
    seed_file(&cache, "a.bin", &vec![1u8; 30]);
    seed_file(&nested, "b.bin", &vec![1u8; 70]);

    let freed = clear_cache_under(&cache).expect("clear ok");
    assert_eq!(freed, 100);
    assert!(cache.exists(), "cache dir itself must survive the wipe");
    assert!(!nested.exists(), "child dir must be removed");
    assert!(!cache.join("a.bin").exists(), "child file must be removed");
}

#[test]
fn clear_cache_on_empty_dir_returns_zero() {
    let ws = workspace("clear-empty");
    let cache = ws.path().join("cache");
    fs::create_dir_all(&cache).expect("mkdir");
    let freed = clear_cache_under(&cache).expect("clear ok");
    assert_eq!(freed, 0);

    // Calling on a never-created dir also succeeds with zero — idempotent.
    let missing = ws.path().join("ghost");
    let freed2 = clear_cache_under(&missing).expect("ghost ok");
    assert_eq!(freed2, 0);
}

// ─── AC-3: export ────────────────────────────────────────────────────────────

#[test]
fn export_then_import_recovers_files() {
    let ws = workspace("export-roundtrip");
    let src = ws.path().join("sessions-src");
    let dest = ws.path().join("sessions-dest");
    let tarball = ws.path().join("out.tar.gz");

    seed_file(&src, "a.json", b"{\"hello\":1}");
    seed_file(&src.join("sub"), "b.bin", &vec![0xAA; 32]);

    let bytes = export_sessions_under(&src, &tarball).expect("export ok");
    assert!(bytes > 0, "should report packed bytes");
    assert!(tarball.exists(), "tarball must exist on disk");

    let imported = import_sessions_under(&tarball, &dest).expect("import ok");
    // 2 files (excluding directory entries — tar emits separate dir
    // headers but our importer increments only on files).
    assert_eq!(imported, 2);
    assert_eq!(
        fs::read_to_string(dest.join("a.json")).unwrap(),
        "{\"hello\":1}"
    );
    assert_eq!(fs::read(dest.join("sub/b.bin")).unwrap(), vec![0xAA; 32]);
}

#[test]
fn export_handles_empty_sessions_dir() {
    let ws = workspace("export-empty");
    // src deliberately does not exist.
    let src = ws.path().join("nope");
    let tarball = ws.path().join("empty.tar.gz");

    let bytes = export_sessions_under(&src, &tarball).expect("export ok");
    assert_eq!(bytes, 0);
    assert!(tarball.exists());
}

// ─── AC-4: import safety ─────────────────────────────────────────────────────

#[test]
fn import_rejects_zip_slip_paths() {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::{Builder, Header};

    let ws = workspace("zip-slip");
    let dest = ws.path().join("dest");
    let tarball = ws.path().join("evil.tar.gz");

    // Craft a tarball with one safe entry and one ../../escape entry.
    let file = fs::File::create(&tarball).unwrap();
    let enc = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(enc);

    let safe_bytes = b"ok";
    let mut h1 = Header::new_gnu();
    h1.set_path("sessions/safe.txt").unwrap();
    h1.set_size(safe_bytes.len() as u64);
    h1.set_cksum();
    builder
        .append(&h1, std::io::Cursor::new(&safe_bytes[..]))
        .unwrap();

    let evil_bytes = b"pwn";
    let mut h2 = Header::new_gnu();
    h2.set_path("sessions/../../escape.txt").unwrap();
    h2.set_size(evil_bytes.len() as u64);
    h2.set_cksum();
    builder
        .append(&h2, std::io::Cursor::new(&evil_bytes[..]))
        .unwrap();
    builder.finish().unwrap();

    let imported = import_sessions_under(&tarball, &dest).expect("import ok");
    // Only the safe entry should land. Even if tar produces extra dir
    // entries we should never see the escape file outside `dest`.
    assert_eq!(imported, 1);
    assert!(dest.join("safe.txt").exists());

    let escaped = ws.path().join("escape.txt");
    assert!(
        !escaped.exists(),
        "zip-slip target must not be written outside dest"
    );
}

#[test]
fn import_fails_on_missing_tarball() {
    let ws = workspace("import-missing");
    let dest = ws.path().join("dest");
    let phantom = ws.path().join("ghost.tar.gz");
    let result = import_sessions_under(&phantom, &dest);
    assert!(result.is_err(), "import must error on missing tarball");
}
