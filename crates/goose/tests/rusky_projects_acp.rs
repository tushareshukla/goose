//! Integration tests for the `_rusky/projects/create` ACP method.
//!
//! These exercise the free-function helpers exposed by
//! `goose::acp::server::rusky_projects`. The full `GooseAcpAgent`
//! constructor is not callable from a unit test (it needs a provider
//! factory, session manager, and on-disk config), so the filesystem-
//! touching happy path runs through `create_project_at()` with a
//! caller-supplied `tempfile::TempDir`.
//!
//! The validation-only branches are already covered by the unit tests
//! in `rusky_projects::tests`; this suite focuses on the end-to-end
//! filesystem behaviour.

use goose::acp::server::rusky_projects::{
    create_project_at, resolve_parent_dir, slugify, validate_name, ProjectMarker,
};
use std::path::PathBuf;

#[test]
fn creates_project_directory_and_marker() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let parent = tmp.path().to_path_buf();
    let name = validate_name("Marketing Site").expect("name is valid");

    let (project_id, root) = create_project_at(&parent, name).expect("create ok");

    assert!(root.starts_with(&parent), "root must be inside parent");
    assert!(root.is_dir(), "project directory must exist on disk");

    let marker_path = root.join(".rusky-project.json");
    assert!(marker_path.is_file(), "marker file must exist");

    let bytes = std::fs::read(&marker_path).expect("read marker");
    let marker: ProjectMarker = serde_json::from_slice(&bytes).expect("parse marker");
    assert_eq!(marker.id, project_id);
    assert_eq!(marker.name, "Marketing Site");
    // RFC-3339 timestamps include a `T` between date and time.
    assert!(marker.created_at.contains('T'), "timestamp shape");

    // UUID v4 string has 36 chars including dashes.
    assert_eq!(project_id.len(), 36);
}

#[test]
fn collision_appends_numeric_suffix() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let parent = tmp.path().to_path_buf();

    let name = validate_name("Project Alpha").unwrap();
    let (_id1, root1) = create_project_at(&parent, name).expect("first ok");
    let (_id2, root2) = create_project_at(&parent, name).expect("second ok");
    let (_id3, root3) = create_project_at(&parent, name).expect("third ok");

    assert_ne!(root1, root2, "second project must land in a different folder");
    assert_ne!(root2, root3, "third project must land in a different folder");

    // The first project owns the canonical slug; subsequent ones get
    // `-2`, `-3`, … suffixes.
    assert_eq!(
        root1.file_name().and_then(std::ffi::OsStr::to_str),
        Some("project-alpha")
    );
    assert_eq!(
        root2.file_name().and_then(std::ffi::OsStr::to_str),
        Some("project-alpha-2")
    );
    assert_eq!(
        root3.file_name().and_then(std::ffi::OsStr::to_str),
        Some("project-alpha-3")
    );
}

#[test]
fn non_ascii_name_falls_back_to_project_slug() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let parent = tmp.path().to_path_buf();

    // Two distinct display names that both slugify to "project" — they
    // should still land in distinct folders.
    let n1 = validate_name("漢字プロジェクト").unwrap();
    let n2 = validate_name("日本語の名前").unwrap();

    let (_id1, root1) = create_project_at(&parent, n1).expect("first ok");
    let (_id2, root2) = create_project_at(&parent, n2).expect("second ok");

    assert_eq!(slugify(n1), "project");
    assert_eq!(slugify(n2), "project");
    assert_eq!(root1.file_name().and_then(std::ffi::OsStr::to_str), Some("project"));
    assert_eq!(root2.file_name().and_then(std::ffi::OsStr::to_str), Some("project-2"));
}

#[test]
fn each_project_gets_unique_id() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let parent = tmp.path().to_path_buf();

    let name = validate_name("Repeated Name").unwrap();
    let (id1, _) = create_project_at(&parent, name).expect("first ok");
    let (id2, _) = create_project_at(&parent, name).expect("second ok");
    assert_ne!(id1, id2, "UUIDs must be unique per project");
}

#[test]
fn resolve_parent_accepts_absolute_and_rejects_relative() {
    // Round-trip through the same helper the handler uses to confirm
    // the contract that the FE relies on.
    let abs: PathBuf = if cfg!(windows) {
        PathBuf::from("C:\\tmp\\rusky-test")
    } else {
        PathBuf::from("/tmp/rusky-test")
    };
    let resolved =
        resolve_parent_dir(Some(abs.to_str().unwrap())).expect("absolute is accepted");
    assert_eq!(resolved, abs);

    let err = resolve_parent_dir(Some("relative/path"))
        .expect_err("relative path is rejected");
    let v = serde_json::to_value(&err).expect("err serializes");
    assert_eq!(
        v.get("code").and_then(|c| c.as_i64()),
        Some(-32602),
        "must surface invalid_params, not internal_error"
    );
}
