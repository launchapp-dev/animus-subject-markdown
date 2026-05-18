//! Deterministic, atomic Markdown subject file writer.
//!
//! The writer formats a [`MarkdownDoc`] to a fixed shape:
//!
//! - Frontmatter rendered by `serde_yaml::to_string`, which honors struct
//!   field declaration order in 0.9.
//! - LF line endings only (`\n`), no `\r\n`.
//! - Exactly one trailing newline on the body.
//! - Body never indented; freeform Markdown.
//!
//! Writes go to a sibling `*.tmp` file in the same directory first, then are
//! atomically renamed into place — so a crash mid-write never leaves a torn
//! file.

use std::path::Path;

use anyhow::{Context, Result};

use crate::parser::{Frontmatter, MarkdownDoc};

/// Render a [`MarkdownDoc`] to its canonical on-disk string form.
///
/// The exact output is `"---\n" + serde_yaml::to_string(frontmatter) + "---\n" + body`
/// with the body normalized to end in exactly one `\n`. An empty body still
/// produces a `\n` so every file ends in a newline.
pub fn render(doc: &MarkdownDoc) -> Result<String> {
    let yaml = render_frontmatter(&doc.frontmatter)?;
    let body = normalize_body(&doc.body);
    Ok(format!("---\n{yaml}---\n{body}"))
}

/// Render just the frontmatter half — `serde_yaml::to_string` plus a guarantee
/// that the output ends with a single `\n`.
pub fn render_frontmatter(frontmatter: &Frontmatter) -> Result<String> {
    let mut yaml = serde_yaml::to_string(frontmatter).context("serialize frontmatter")?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    Ok(yaml)
}

/// Normalize the body: convert CRLF to LF, strip trailing blank lines, and
/// guarantee exactly one terminating `\n`.
pub fn normalize_body(body: &str) -> String {
    let lf = body.replace("\r\n", "\n");
    let trimmed = lf.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::from("\n")
    } else {
        let mut out = String::with_capacity(trimmed.len() + 1);
        out.push_str(trimmed);
        out.push('\n');
        out
    }
}

/// Write `contents` to `path` atomically by staging in a sibling `*.tmp` file
/// and renaming. The parent directory is created if missing.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    let tmp = tmp_path_for(path);
    std::fs::write(&tmp, contents.as_bytes())
        .with_context(|| format!("writing tmp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Compute the sibling `*.tmp` staging path for an atomic write target.
pub fn tmp_path_for(path: &Path) -> std::path::PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => std::path::PathBuf::from(name),
    }
}

/// Render + atomic-write a doc in one call.
pub fn render_and_write(path: &Path, doc: &MarkdownDoc) -> Result<()> {
    let rendered = render(doc)?;
    write_atomic(path, &rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{self, Frontmatter};
    use animus_subject_protocol::SubjectStatus;
    use tempfile::TempDir;

    fn sample_doc() -> MarkdownDoc {
        let fm = Frontmatter::new(
            "markdown:TASK-0001",
            "task",
            "Hello",
            SubjectStatus::Ready,
            "2026-05-18T12:00:00Z".parse().unwrap(),
            "2026-05-18T12:00:00Z".parse().unwrap(),
        );
        MarkdownDoc {
            frontmatter: fm,
            body: "body\n".into(),
        }
    }

    #[test]
    fn render_round_trips_through_parse() {
        let doc = sample_doc();
        let rendered = render(&doc).unwrap();
        let parsed = parser::parse(&rendered).unwrap();
        assert_eq!(parsed, doc);
    }

    #[test]
    fn render_is_byte_identical_for_same_input() {
        let doc = sample_doc();
        let a = render(&doc).unwrap();
        let b = render(&doc).unwrap();
        assert_eq!(a, b, "render must be deterministic");
    }

    #[test]
    fn normalize_body_yields_single_trailing_newline() {
        assert_eq!(normalize_body("hi"), "hi\n");
        assert_eq!(normalize_body("hi\n"), "hi\n");
        assert_eq!(normalize_body("hi\n\n\n"), "hi\n");
        assert_eq!(normalize_body(""), "\n");
        assert_eq!(normalize_body("hi\r\nworld\r\n"), "hi\nworld\n");
    }

    #[test]
    fn write_atomic_creates_parent_and_writes_contents() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("nested/sub/file.md");
        write_atomic(&target, "hello\n").unwrap();
        let got = std::fs::read_to_string(&target).unwrap();
        assert_eq!(got, "hello\n");
    }
}
