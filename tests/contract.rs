//! Contract tests for the Markdown `SubjectBackend` implementation.
//!
//! Each test stands up a `tempfile::TempDir`, points a [`MarkdownBackend`] at
//! it, and exercises one trait method end-to-end. Fixtures live in
//! `tests/fixtures/` so the assertions stay focused on mapping logic.

use std::time::Duration;

use animus_plugin_protocol::HealthStatus;
use animus_subject_markdown::backend::MarkdownBackend;
use animus_subject_markdown::config::MarkdownConfig;
use animus_subject_markdown::id_gen::SequenceGenerator;
use animus_subject_markdown::parser::{self, Frontmatter, MarkdownDoc};
use animus_subject_markdown::writer;
use animus_subject_protocol::{
    BackendError, Subject, SubjectBackend, SubjectFilter, SubjectId, SubjectPatch, SubjectStatus,
};
use futures::stream::StreamExt;
use tempfile::TempDir;
use tokio::time::timeout;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> String {
    let path = format!("{FIXTURE_DIR}/{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

fn temp_backend(kinds: &[&str]) -> (TempDir, MarkdownBackend) {
    let tmp = TempDir::new().unwrap();
    let cfg = MarkdownConfig::new(
        tmp.path(),
        kinds.iter().map(|s| (*s).to_string()).collect(),
        "markdown:",
    );
    let backend = MarkdownBackend::new(cfg);
    (tmp, backend)
}

// =====================================================================
// 1. parse_frontmatter_round_trips
// =====================================================================

#[test]
fn parse_frontmatter_round_trips() {
    let raw = fixture("task-001.md");
    let doc = parser::parse(&raw).expect("fixture must parse");
    let rendered = writer::render(&doc).expect("must render");
    assert_eq!(
        rendered, raw,
        "render(parse(fixture)) must be byte-identical to fixture"
    );
}

// =====================================================================
// 2. body_preserved_through_update
// =====================================================================

#[tokio::test]
async fn body_preserved_through_update() {
    let (tmp, backend) = temp_backend(&["task"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();
    std::fs::write(tmp.path().join("task/TASK-0001.md"), fixture("task-001.md")).unwrap();

    let id = SubjectId::new("markdown:TASK-0001");
    let before = backend.get(&id).await.expect("get before");
    let updated = backend
        .update(
            &id,
            SubjectPatch {
                status: Some(SubjectStatus::Done),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.status, SubjectStatus::Done);
    assert_eq!(
        updated.description, before.description,
        "body must survive a frontmatter-only patch"
    );
}

// =====================================================================
// 3. create_writes_deterministic_file_format
// =====================================================================

#[test]
fn create_writes_deterministic_file_format() {
    // Build the same doc twice and assert byte-identical output. This is the
    // hard requirement that makes the storage git-friendly.
    let fm = Frontmatter::new(
        "markdown:TASK-0001",
        "task",
        "Hello",
        SubjectStatus::Ready,
        "2026-05-18T12:00:00Z".parse().unwrap(),
        "2026-05-18T12:00:00Z".parse().unwrap(),
    );
    let doc = MarkdownDoc {
        frontmatter: fm,
        body: "body\n".into(),
    };
    let a = writer::render(&doc).unwrap();
    let b = writer::render(&doc).unwrap();
    assert_eq!(a, b);
    assert!(a.ends_with('\n'), "file must end with newline");
    assert!(!a.contains("\r\n"), "no CRLF line endings allowed");
}

// =====================================================================
// 4. update_merges_frontmatter_without_touching_body
// =====================================================================

#[tokio::test]
async fn update_merges_frontmatter_without_touching_body() {
    let (tmp, backend) = temp_backend(&["task"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();
    std::fs::write(
        tmp.path().join("task/TASK-0042.md"),
        fixture("task-with-body.md"),
    )
    .unwrap();

    let id = SubjectId::new("markdown:TASK-0042");
    let before_raw = std::fs::read_to_string(tmp.path().join("task/TASK-0042.md")).unwrap();
    let body_before = parser::parse(&before_raw).unwrap().body;

    let updated = backend
        .update(
            &id,
            SubjectPatch {
                labels_add: vec!["q2".into()],
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert!(updated.labels.contains(&"q2".into()));

    let after_raw = std::fs::read_to_string(tmp.path().join("task/TASK-0042.md")).unwrap();
    let body_after = parser::parse(&after_raw).unwrap().body;
    assert_eq!(
        body_after, body_before,
        "body must be untouched by a label-only patch"
    );
}

// =====================================================================
// 5. update_appends_to_body_when_requested
// =====================================================================

#[tokio::test]
async fn update_appends_to_body_when_requested() {
    let (tmp, backend) = temp_backend(&["task"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();
    std::fs::write(
        tmp.path().join("task/TASK-0042.md"),
        fixture("task-with-body.md"),
    )
    .unwrap();

    let id = SubjectId::new("markdown:TASK-0042");
    backend
        .update(
            &id,
            SubjectPatch {
                comment: Some("Triaged by alice — ready for impl.".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update");

    let after = std::fs::read_to_string(tmp.path().join("task/TASK-0042.md")).unwrap();
    assert!(
        after.contains("> Triaged by alice"),
        "comment must be appended as a Markdown blockquote"
    );
}

// =====================================================================
// 6. list_walks_subdirs_by_kind
// =====================================================================

#[tokio::test]
async fn list_walks_subdirs_by_kind() {
    let (tmp, backend) = temp_backend(&["task", "issue"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();
    std::fs::create_dir_all(tmp.path().join("issue")).unwrap();
    std::fs::write(tmp.path().join("task/TASK-0001.md"), fixture("task-001.md")).unwrap();
    std::fs::write(
        tmp.path().join("issue/ISSUE-0001.md"),
        fixture("task-001.md")
            .replace("markdown:TASK-0001", "markdown:ISSUE-0001")
            .replace("kind: task", "kind: issue"),
    )
    .unwrap();

    let page = backend
        .list(SubjectFilter::default())
        .await
        .expect("list must walk both kinds");
    assert_eq!(page.subjects.len(), 2);
    let ids: Vec<String> = page.subjects.iter().map(|s| s.id.to_string()).collect();
    assert!(ids.contains(&"markdown:TASK-0001".to_string()));
    assert!(ids.contains(&"markdown:ISSUE-0001".to_string()));
}

// =====================================================================
// 7. list_filters_by_status
// =====================================================================

#[tokio::test]
async fn list_filters_by_status() {
    let (tmp, backend) = temp_backend(&["task"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();
    // fixture task-001 has status: in-progress, task-with-body has status: ready
    std::fs::write(tmp.path().join("task/TASK-0001.md"), fixture("task-001.md")).unwrap();
    std::fs::write(
        tmp.path().join("task/TASK-0042.md"),
        fixture("task-with-body.md"),
    )
    .unwrap();

    let page = backend
        .list(SubjectFilter {
            status: vec![SubjectStatus::Ready],
            ..Default::default()
        })
        .await
        .expect("filtered list");
    assert_eq!(page.subjects.len(), 1);
    assert_eq!(page.subjects[0].id.as_str(), "markdown:TASK-0042");
}

// =====================================================================
// 8. id_generation_is_sequential
// =====================================================================

#[tokio::test]
async fn id_generation_is_sequential() {
    let (tmp, backend) = temp_backend(&["task"]);
    let _ = tmp; // keep tempdir alive

    let a: Subject = backend
        .create_subject("task", "First", None)
        .await
        .expect("create 1");
    let b: Subject = backend
        .create_subject("task", "Second", None)
        .await
        .expect("create 2");
    let c: Subject = backend
        .create_subject("task", "Third", None)
        .await
        .expect("create 3");

    assert_eq!(a.id.as_str(), "markdown:TASK-0001");
    assert_eq!(b.id.as_str(), "markdown:TASK-0002");
    assert_eq!(c.id.as_str(), "markdown:TASK-0003");
}

// =====================================================================
// 9. id_generation_handles_gaps
// =====================================================================

#[test]
fn id_generation_handles_gaps() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("TASK-0001.md"), "").unwrap();
    std::fs::write(tmp.path().join("TASK-0003.md"), "").unwrap();
    let gen = SequenceGenerator::discover(tmp.path(), "TASK");
    // Gap at 0002 must NOT be filled; next is one past the max existing.
    assert_eq!(gen.next(), 4);
}

// =====================================================================
// 10. watch_emits_event_on_external_edit
// =====================================================================

#[tokio::test]
async fn watch_emits_event_on_external_edit() {
    let (tmp, backend) = temp_backend(&["task"]);
    std::fs::create_dir_all(tmp.path().join("task")).unwrap();

    let mut stream = backend.watch().await.expect("backend must implement watch");

    // Give the watcher a beat to wire up before we touch the FS.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Write a brand-new subject file directly to the FS (i.e. "external edit").
    let body = fixture("task-001.md");
    std::fs::write(tmp.path().join("task/TASK-0001.md"), &body).unwrap();

    let event = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("watch must fire within 5s")
        .expect("stream must yield at least one event");
    assert_eq!(event.id.as_str(), "markdown:TASK-0001");
    // Drop the stream so the watcher task can shut down.
    drop(stream);
}

// =====================================================================
// 11. schema_advertises_supports_create_true
// =====================================================================

#[test]
fn schema_advertises_supports_create_true() {
    let (_tmp, backend) = temp_backend(&["task"]);
    let schema = backend.schema();
    assert!(schema.supports_create);
    assert!(schema.supports_watch);
    assert!(!schema.supports_pagination);
    assert_eq!(schema.kinds, vec!["task".to_string()]);
}

// =====================================================================
// 12. health_unhealthy_when_root_dir_not_writable
// =====================================================================

#[cfg(unix)]
#[tokio::test]
async fn health_unhealthy_when_root_dir_not_writable() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("subjects");
    std::fs::create_dir_all(&root).unwrap();
    // Chmod 0o555: readable+executable but NOT writable.
    let mut perms = std::fs::metadata(&root).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&root, perms.clone()).unwrap();

    let cfg = MarkdownConfig::new(&root, vec!["task".into()], "markdown:");
    let backend = MarkdownBackend::new(cfg);

    let health = backend.health().await.expect("health does not error");

    // Restore permissions so TempDir's Drop can clean up.
    let mut restore = std::fs::metadata(&root).unwrap().permissions();
    restore.set_mode(0o755);
    std::fs::set_permissions(&root, restore).unwrap();

    // Skip the assertion if the test process happens to be running as root
    // (the chmod is a no-op for uid 0 on many systems).
    if nix_running_as_root() {
        eprintln!("skipping non-writable assertion: running as root");
        return;
    }
    assert_eq!(health.status, HealthStatus::Unhealthy);
    assert!(health.last_error.is_some());
}

#[cfg(unix)]
fn nix_running_as_root() -> bool {
    // SAFETY: getuid() is async-signal-safe and never fails.
    unsafe { libc_getuid() == 0 }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

// =====================================================================
// Bonus: bad-id-shape rejection (uses NotFound for missing files,
// InvalidRequest for malformed ids).
// =====================================================================

#[tokio::test]
async fn get_rejects_malformed_id() {
    let (_tmp, backend) = temp_backend(&["task"]);
    let err = backend
        .get(&SubjectId::new("notmarkdown:WHATEVER"))
        .await
        .expect_err("malformed id must error");
    assert!(matches!(err, BackendError::InvalidRequest(_)));
}

#[tokio::test]
async fn get_returns_not_found_for_missing_file() {
    let (_tmp, backend) = temp_backend(&["task"]);
    let err = backend
        .get(&SubjectId::new("markdown:TASK-9999"))
        .await
        .expect_err("missing file must error");
    assert!(matches!(err, BackendError::NotFound(_)));
}
