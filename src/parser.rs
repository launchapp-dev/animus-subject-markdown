//! Parse + render a `(frontmatter, body)` pair from/to a Markdown file.
//!
//! The on-disk format is a `---`-delimited YAML frontmatter block followed by a
//! Markdown body. The frontmatter is a strict, explicitly-ordered struct
//! ([`Frontmatter`]) so two writes of the same logical content produce
//! byte-identical files — a hard requirement for git-friendly storage.
//!
//! Frontmatter field order is fixed (declaration order on the struct);
//! `serde_yaml` honors struct field order in 0.9. Top-level fields with
//! default values (e.g. `parent_id: null`, `labels: []`, `attachments: []`,
//! `custom_fields: {}`) are always written so a `git diff` that adds a label
//! shows up as a change on a known line rather than the appearance of a new
//! key.

use std::collections::BTreeMap;

use animus_subject_protocol::{SubjectAttachment, SubjectStatus};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Strict, ordered YAML frontmatter for one Markdown-backed subject.
///
/// Field declaration order is the on-disk write order. Do NOT reorder fields
/// in this struct without bumping the on-disk format version — every subject
/// file in every consuming repo will re-diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Fully-qualified subject id, e.g. `markdown:TASK-0001`.
    pub id: String,

    /// Subject kind. Backend-defined; default `"task"`.
    pub kind: String,

    /// Short human-readable title.
    pub title: String,

    /// Normalized status as the kebab-case wire string (`ready`, `in-progress`,
    /// `blocked`, `done`, `cancelled`).
    pub status: SubjectStatus,

    /// Optional priority on a 0..=4 scale, or null.
    #[serde(default)]
    pub priority: Option<u8>,

    /// Free-form assignee identifier, or null.
    #[serde(default)]
    pub assignee: Option<String>,

    /// Labels / tags. Always serialized as a list (possibly empty).
    #[serde(default)]
    pub labels: Vec<String>,

    /// Parent subject id, or null.
    #[serde(default)]
    pub parent_id: Option<String>,

    /// Created timestamp, RFC3339.
    pub created_at: DateTime<Utc>,

    /// Updated timestamp, RFC3339.
    pub updated_at: DateTime<Utc>,

    /// Backend-raw status string (e.g. `"In Progress"`), or null.
    #[serde(default)]
    pub native_status: Option<String>,

    /// Workflow dispatch label, or null.
    #[serde(default)]
    pub dispatch_label: Option<String>,

    /// Free-form backend status payload. Always present (default `{}`).
    #[serde(default = "default_status_metadata")]
    pub status_metadata: Value,

    /// Attachments. Always present (default `[]`).
    #[serde(default)]
    pub attachments: Vec<SubjectAttachment>,

    /// Backend-specific custom fields. Stable ordering via [`BTreeMap`] so
    /// the rendered YAML is deterministic.
    #[serde(default)]
    pub custom_fields: BTreeMap<String, Value>,
}

fn default_status_metadata() -> Value {
    Value::Object(serde_json::Map::new())
}

impl Frontmatter {
    /// Build a frontmatter shell with the structural fields filled in and
    /// every optional field defaulted to its empty form.
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
        status: SubjectStatus,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            status,
            priority: None,
            assignee: None,
            labels: Vec::new(),
            parent_id: None,
            created_at,
            updated_at,
            native_status: None,
            dispatch_label: None,
            status_metadata: default_status_metadata(),
            attachments: Vec::new(),
            custom_fields: BTreeMap::new(),
        }
    }
}

/// A parsed Markdown subject file — frontmatter plus body. The body retains
/// its original newline structure (the body's trailing newlines are NOT
/// trimmed during parsing; the writer normalizes them on serialize).
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownDoc {
    /// Parsed YAML frontmatter.
    pub frontmatter: Frontmatter,
    /// Markdown body, post-`---` line. May be empty.
    pub body: String,
}

/// Parse the contents of a Markdown subject file.
///
/// The expected shape is:
///
/// ```text
/// ---\n
/// <yaml>\n
/// ---\n
/// <body>
/// ```
///
/// Both `\n` and `\r\n` line endings are accepted on read; the writer always
/// emits `\n`.
pub fn parse(input: &str) -> Result<MarkdownDoc> {
    // Normalize to \n internally so the rest of the parser stays simple. We
    // strip a BOM too in case some editor wrote one.
    let trimmed = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let normalized = trimmed.replace("\r\n", "\n");

    let rest = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("missing opening `---` frontmatter delimiter"))?;

    // Split into yaml + body on the first standalone `---\n` line.
    let (yaml_text, body) = split_on_delimiter(rest)?;

    let frontmatter: Frontmatter =
        serde_yaml::from_str(yaml_text).context("failed to parse YAML frontmatter")?;

    Ok(MarkdownDoc {
        frontmatter,
        body: body.to_string(),
    })
}

/// Locate the closing `---` line and split the buffer into `(yaml, body)`.
/// The closing delimiter and its trailing newline are removed; everything
/// after is treated as the body.
fn split_on_delimiter(rest: &str) -> Result<(&str, &str)> {
    let mut search_from = 0usize;
    while let Some(rel) = rest[search_from..].find("---") {
        let abs = search_from + rel;
        // Must be at a line boundary — either start of string or preceded by \n.
        let at_line_start = abs == 0 || rest.as_bytes()[abs - 1] == b'\n';
        // Must be followed by \n or end-of-string.
        let next = abs + 3;
        let at_line_end = next == rest.len() || rest.as_bytes()[next] == b'\n';
        if at_line_start && at_line_end {
            let yaml = &rest[..abs];
            let body_start = if next < rest.len() { next + 1 } else { next };
            let body = if body_start <= rest.len() {
                &rest[body_start..]
            } else {
                ""
            };
            return Ok((yaml.trim_end_matches('\n'), body));
        }
        search_from = abs + 3;
    }
    Err(anyhow!("missing closing `---` frontmatter delimiter"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Frontmatter {
        let mut fm = Frontmatter::new(
            "markdown:TASK-0001",
            "task",
            "Fix the login bug",
            SubjectStatus::InProgress,
            "2026-05-18T12:00:00Z".parse().unwrap(),
            "2026-05-18T13:30:00Z".parse().unwrap(),
        );
        fm.priority = Some(3);
        fm.assignee = Some("alice@example.com".into());
        fm.labels = vec!["auth".into(), "p1".into()];
        fm.native_status = Some("In Progress".into());
        fm.dispatch_label = Some("code-review".into());
        fm.custom_fields
            .insert("cycle".into(), json!("2026-Q2-cycle-1"));
        fm
    }

    #[test]
    fn parse_basic_doc() {
        let raw = "---\nid: markdown:TASK-0001\nkind: task\ntitle: Hello\nstatus: ready\ncreated_at: 2026-05-18T12:00:00Z\nupdated_at: 2026-05-18T12:00:00Z\n---\nbody here\n";
        let doc = parse(raw).unwrap();
        assert_eq!(doc.frontmatter.id, "markdown:TASK-0001");
        assert_eq!(doc.frontmatter.title, "Hello");
        assert_eq!(doc.frontmatter.status, SubjectStatus::Ready);
        assert_eq!(doc.body, "body here\n");
    }

    #[test]
    fn parse_rejects_missing_opening_delim() {
        let raw = "id: foo\n---\nbody\n";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn parse_rejects_missing_closing_delim() {
        let raw = "---\nid: x\nkind: task\ntitle: x\nstatus: ready\ncreated_at: 2026-05-18T12:00:00Z\nupdated_at: 2026-05-18T12:00:00Z\nbody-without-delim";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn parse_accepts_crlf_line_endings() {
        let raw = "---\r\nid: markdown:TASK-0001\r\nkind: task\r\ntitle: Hi\r\nstatus: ready\r\ncreated_at: 2026-05-18T12:00:00Z\r\nupdated_at: 2026-05-18T12:00:00Z\r\n---\r\nbody\r\n";
        let doc = parse(raw).unwrap();
        assert_eq!(doc.frontmatter.id, "markdown:TASK-0001");
        // After normalization, the body uses \n endings.
        assert_eq!(doc.body, "body\n");
    }

    #[test]
    fn parse_round_trips_sample() {
        // Serialize a sample frontmatter via serde_yaml + glue, parse it back,
        // and assert equality. The writer module exercises full byte-level
        // determinism; this just checks structural identity.
        let fm = sample();
        let yaml = serde_yaml::to_string(&fm).unwrap();
        let raw = format!("---\n{yaml}---\nbody!\n");
        let doc = parse(&raw).unwrap();
        assert_eq!(doc.frontmatter, fm);
        assert_eq!(doc.body, "body!\n");
    }
}
