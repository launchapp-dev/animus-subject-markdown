//! Diagnostic helper: prints the canonical render of a sample frontmatter to
//! stdout when run via `cargo test render_probe::dump -- --nocapture`. Used
//! once to seed fixture files; not part of the regular contract suite (the
//! function only emits output, it never asserts).

use animus_subject_markdown::parser::{Frontmatter, MarkdownDoc};
use animus_subject_markdown::writer;
use animus_subject_protocol::SubjectStatus;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn dump() {
    let fm = Frontmatter {
        id: "markdown:TASK-0001".into(),
        kind: "task".into(),
        title: "Fix the login bug".into(),
        status: SubjectStatus::InProgress,
        priority: Some(3),
        assignee: Some("alice@example.com".into()),
        labels: vec!["auth".into(), "p1".into()],
        parent_id: None,
        created_at: "2026-05-18T12:00:00Z".parse().unwrap(),
        updated_at: "2026-05-18T13:30:00Z".parse().unwrap(),
        native_status: Some("In Progress".into()),
        dispatch_label: Some("code-review".into()),
        status_metadata: json!({}),
        attachments: Vec::new(),
        custom_fields: {
            let mut m = BTreeMap::new();
            m.insert("cycle".into(), json!("2026-Q2-cycle-1"));
            m
        },
    };
    let doc = MarkdownDoc {
        frontmatter: fm,
        body: "# Fix the login bug\n\nDescription goes here.\n".into(),
    };
    println!("---BEGIN RENDER---");
    print!("{}", writer::render(&doc).unwrap());
    println!("---END RENDER---");
}
