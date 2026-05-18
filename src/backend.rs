//! [`MarkdownBackend`] — the `SubjectBackend` implementation backed by one
//! Markdown file per subject.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_plugin_protocol::{HealthCheckResult, HealthStatus};
use animus_subject_protocol::{
    BackendError, EventStream, Subject, SubjectAttachment, SubjectBackend, SubjectFilter,
    SubjectId, SubjectList, SubjectPatch, SubjectSchema, SubjectStatus,
};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::MarkdownConfig;
use crate::id_gen::{file_name_for, native_id, parse_sequence, SequenceGenerator};
use crate::parser::{self, Frontmatter, MarkdownDoc};
use crate::watcher;
use crate::writer;

/// Markdown-file subject backend.
#[derive(Debug)]
pub struct MarkdownBackend {
    config: MarkdownConfig,
    /// One id generator per kind, keyed by uppercased kind. Built lazily so
    /// the backend can be constructed before the root directory exists.
    sequences: Mutex<HashMap<String, Arc<SequenceGenerator>>>,
}

impl MarkdownBackend {
    /// Build a backend from configuration. Does NOT touch the filesystem.
    pub fn new(config: MarkdownConfig) -> Self {
        Self {
            config,
            sequences: Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the configuration this backend was built with.
    pub fn config(&self) -> &MarkdownConfig {
        &self.config
    }

    /// Resolve or lazily create the [`SequenceGenerator`] for `kind`.
    async fn sequence_for(&self, kind: &str) -> Arc<SequenceGenerator> {
        let kind_upper = kind.to_ascii_uppercase();
        let mut guard = self.sequences.lock().await;
        if let Some(gen) = guard.get(&kind_upper) {
            return gen.clone();
        }
        let dir = self.config.kind_dir(kind);
        let gen = Arc::new(SequenceGenerator::discover(&dir, &kind_upper));
        guard.insert(kind_upper, gen.clone());
        gen
    }

    /// Strip the configured id prefix and return the native id portion
    /// (e.g. `markdown:TASK-0001` -> `TASK-0001`).
    fn native_id<'a>(&self, id: &'a SubjectId) -> Result<&'a str, BackendError> {
        let raw = id.as_str();
        raw.strip_prefix(&self.config.id_prefix).ok_or_else(|| {
            BackendError::InvalidRequest(format!(
                "subject id {raw:?} is not a markdown id (expected `{}` prefix)",
                self.config.id_prefix
            ))
        })
    }

    /// Compute the on-disk path for a fully-qualified [`SubjectId`].
    pub fn path_for(&self, id: &SubjectId) -> Result<PathBuf, BackendError> {
        let native = self.native_id(id)?;
        let (kind_upper, _seq) = split_native(native)?;
        let kind_lower = kind_upper.to_ascii_lowercase();
        // Allow any configured kind whose upper-cased form matches.
        let kind = self
            .config
            .kinds
            .iter()
            .find(|k| k.eq_ignore_ascii_case(&kind_lower))
            .cloned()
            .unwrap_or(kind_lower);
        Ok(self.config.kind_dir(&kind).join(format!("{native}.md")))
    }

    /// Read + parse + map a Markdown file at `path` into a [`Subject`].
    pub async fn read_subject_at(&self, path: &Path) -> Result<Subject, BackendError> {
        let path = path.to_path_buf();
        let raw = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| BackendError::Other(anyhow!("read {}: {e}", path.display())))?;
        let doc = parser::parse(&raw)
            .with_context(|| format!("parse {}", path.display()))
            .map_err(BackendError::Other)?;
        Ok(subject_from_doc(doc))
    }

    /// Create a new subject of `kind` with the given title and description.
    ///
    /// Allocates the next sequence id, writes the file atomically, and
    /// returns the materialized [`Subject`]. Default fields are filled in to
    /// match the empty-form shape (`labels=[]`, `attachments=[]`,
    /// `status_metadata={}`, `custom_fields={}`).
    pub async fn create_subject(
        &self,
        kind: &str,
        title: impl Into<String>,
        description: Option<String>,
    ) -> Result<Subject, BackendError> {
        let kind = kind.to_string();
        if !self
            .config
            .kinds
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&kind))
        {
            return Err(BackendError::InvalidRequest(format!(
                "kind {kind:?} not in configured kinds {:?}",
                self.config.kinds
            )));
        }
        let kind_upper = kind.to_ascii_uppercase();
        let gen = self.sequence_for(&kind).await;
        let seq = gen.next();
        let native = native_id(&kind_upper, seq);
        let id_str = format!("{}{native}", self.config.id_prefix);

        let now = Utc::now();
        let mut fm = Frontmatter::new(&id_str, &kind, title, SubjectStatus::Ready, now, now);
        // We round-trip the `Subject` shape we'll return, so make sure
        // status_metadata and attachments default to their empty-form values
        // (already set by Frontmatter::new).
        let _ = &mut fm;

        let body = match description.as_deref() {
            Some(d) if !d.is_empty() => format!("{d}\n"),
            _ => String::from("\n"),
        };

        let doc = MarkdownDoc {
            frontmatter: fm,
            body,
        };
        let path = self
            .config
            .kind_dir(&kind)
            .join(file_name_for(&kind_upper, seq));
        writer::render_and_write(&path, &doc).map_err(|e| BackendError::Other(anyhow!("{e}")))?;
        Ok(subject_from_doc(doc))
    }

    /// Walk every configured kind directory and return all parsed subjects.
    /// Files that fail to parse are skipped (with a warning), so a manually
    /// broken file never crashes the daemon's poll loop.
    async fn walk_all(&self) -> Result<Vec<Subject>, BackendError> {
        let mut out = Vec::new();
        for kind in &self.config.kinds {
            let dir = self.config.kind_dir(kind);
            let read = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(BackendError::Other(anyhow!(
                        "read dir {}: {err}",
                        dir.display()
                    )))
                }
            };
            let kind_upper = kind.to_ascii_uppercase();
            for entry in read.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !name.ends_with(".md") || name.ends_with(".md.tmp") {
                    continue;
                }
                if parse_sequence(name, &kind_upper).is_none() {
                    continue;
                }
                match self.read_subject_at(&path).await {
                    Ok(subject) => out.push(subject),
                    Err(err) => tracing::warn!(
                        target: "animus_subject_markdown",
                        ?err,
                        path = %path.display(),
                        "skipping unparseable subject file"
                    ),
                }
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl SubjectBackend for MarkdownBackend {
    async fn list(&self, filter: SubjectFilter) -> Result<SubjectList, BackendError> {
        let all = self.walk_all().await?;
        let subjects = all
            .into_iter()
            .filter(|s| matches_filter(s, &filter))
            .collect();
        Ok(SubjectList {
            subjects,
            next_cursor: None,
            fetched_at: Utc::now(),
        })
    }

    async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> {
        let path = self.path_for(id)?;
        if !path.exists() {
            return Err(BackendError::NotFound(id.to_string()));
        }
        self.read_subject_at(&path).await
    }

    async fn update(&self, id: &SubjectId, patch: SubjectPatch) -> Result<Subject, BackendError> {
        let path = self.path_for(id)?;
        if !path.exists() {
            return Err(BackendError::NotFound(id.to_string()));
        }
        let raw = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| BackendError::Other(anyhow!("read {}: {e}", path.display())))?;
        let mut doc = parser::parse(&raw)
            .with_context(|| format!("parse {}", path.display()))
            .map_err(BackendError::Other)?;

        apply_patch(&mut doc, &patch);
        doc.frontmatter.updated_at = Utc::now();

        writer::render_and_write(&path, &doc).map_err(|e| BackendError::Other(anyhow!("{e}")))?;

        Ok(subject_from_doc(doc))
    }

    async fn watch(&self) -> Option<EventStream> {
        // The runtime hands us `Arc<Self>` already, but our watch helper
        // wants its own reference. We rebuild a thin `Arc<MarkdownBackend>`
        // from the configured root so the watcher task stays self-contained.
        let backend = Arc::new(MarkdownBackend::new(self.config.clone()));
        match watcher::spawn_watch(backend) {
            Ok(stream) => Some(stream),
            Err(err) => {
                tracing::warn!(
                    target: "animus_subject_markdown",
                    ?err,
                    "failed to start file watcher"
                );
                None
            }
        }
    }

    fn schema(&self) -> SubjectSchema {
        SubjectSchema {
            kinds: self.config.kinds.clone(),
            status_values: vec![
                SubjectStatus::Ready,
                SubjectStatus::InProgress,
                SubjectStatus::Blocked,
                SubjectStatus::Done,
                SubjectStatus::Cancelled,
            ],
            supports_watch: true,
            supports_create: true,
            supports_pagination: false,
            native_status_values: Vec::new(),
            status_dispatch_hints: Vec::new(),
            custom_fields: Vec::new(),
        }
    }

    async fn health(&self) -> Result<HealthCheckResult, BackendError> {
        let root = &self.config.root;
        if !root.exists() {
            if let Err(err) = std::fs::create_dir_all(root) {
                return Ok(HealthCheckResult {
                    status: HealthStatus::Unhealthy,
                    uptime_ms: None,
                    memory_usage_bytes: None,
                    last_error: Some(format!("cannot create root {}: {err}", root.display())),
                });
            }
        }
        // Probe writability by staging + cleaning up a tiny sentinel file.
        let probe = root.join(".animus-write-probe");
        match std::fs::write(&probe, b"ok") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                Ok(HealthCheckResult {
                    status: HealthStatus::Healthy,
                    uptime_ms: None,
                    memory_usage_bytes: None,
                    last_error: None,
                })
            }
            Err(err) => Ok(HealthCheckResult {
                status: HealthStatus::Unhealthy,
                uptime_ms: None,
                memory_usage_bytes: None,
                last_error: Some(format!("root {} not writable: {err}", root.display())),
            }),
        }
    }
}

/// Split a native id like `TASK-0001` into `("TASK", 1)`.
fn split_native(native: &str) -> Result<(String, u32), BackendError> {
    let (kind, seq) = native
        .rsplit_once('-')
        .ok_or_else(|| BackendError::InvalidRequest(format!("malformed id {native:?}")))?;
    let seq = seq
        .parse::<u32>()
        .map_err(|_| BackendError::InvalidRequest(format!("malformed id {native:?}")))?;
    if kind.is_empty() {
        return Err(BackendError::InvalidRequest(format!(
            "malformed id {native:?} (empty kind)"
        )));
    }
    Ok((kind.to_string(), seq))
}

/// Translate a parsed [`MarkdownDoc`] into the normalized [`Subject`].
fn subject_from_doc(doc: MarkdownDoc) -> Subject {
    let MarkdownDoc { frontmatter, body } = doc;

    let description = if body.trim().is_empty() {
        None
    } else {
        // Strip the writer's trailing newline so the in-memory body matches
        // what humans typed.
        Some(body.trim_end_matches('\n').to_string())
    };

    // Surface `dispatch_label` and custom_fields through the `custom` map so
    // workflow YAML can template against `{{subject.custom.dispatch_label}}`,
    // `{{subject.custom.cycle}}`, etc. Use a BTreeMap for stable ordering.
    let mut custom: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(label) = frontmatter.dispatch_label.clone() {
        custom.insert("dispatch_label".into(), Value::String(label));
    }
    for (k, v) in frontmatter.custom_fields.iter() {
        custom.insert(k.clone(), v.clone());
    }

    Subject {
        id: SubjectId::new(frontmatter.id.clone()),
        kind: frontmatter.kind.clone(),
        title: frontmatter.title.clone(),
        description,
        status: frontmatter.status,
        priority: frontmatter.priority,
        assignee: frontmatter.assignee.clone(),
        labels: frontmatter.labels.clone(),
        parent: frontmatter.parent_id.as_ref().map(SubjectId::new),
        children: Vec::new(),
        url: None,
        created_at: frontmatter.created_at,
        updated_at: frontmatter.updated_at,
        custom,
        native_status: frontmatter.native_status.clone(),
        status_metadata: frontmatter.status_metadata.clone(),
        attachments: frontmatter.attachments.clone(),
    }
}

fn matches_filter(subject: &Subject, filter: &SubjectFilter) -> bool {
    if !filter.status.is_empty() && !filter.status.contains(&subject.status) {
        return false;
    }
    if !filter.kind.is_empty() && !filter.kind.contains(&subject.kind) {
        return false;
    }
    if !filter.assignee.is_empty() {
        let Some(assignee) = subject.assignee.as_ref() else {
            return false;
        };
        if !filter.assignee.iter().any(|a| a == assignee) {
            return false;
        }
    }
    if !filter.labels_any.is_empty()
        && !filter.labels_any.iter().any(|l| subject.labels.contains(l))
    {
        return false;
    }
    if !filter.labels_all.is_empty()
        && !filter.labels_all.iter().all(|l| subject.labels.contains(l))
    {
        return false;
    }
    if let Some(since) = filter.updated_since {
        if subject.updated_at < since {
            return false;
        }
    }
    if let Some(native) = &filter.native_status {
        match subject.native_status.as_ref() {
            Some(s) if s == native => {}
            _ => return false,
        }
    }
    if let Some(label) = &filter.dispatch_label {
        let on_subject = subject.custom.get("dispatch_label").and_then(Value::as_str);
        if on_subject != Some(label.as_str()) {
            return false;
        }
    }
    if let Some(att_kind) = &filter.has_attachment_kind {
        if !subject
            .attachments
            .iter()
            .any(|a: &SubjectAttachment| &a.kind == att_kind)
        {
            return false;
        }
    }
    true
}

fn apply_patch(doc: &mut MarkdownDoc, patch: &SubjectPatch) {
    if let Some(status) = patch.status {
        doc.frontmatter.status = status;
    }
    if let Some(opt_assignee) = patch.assignee.clone() {
        doc.frontmatter.assignee = opt_assignee;
    }
    if !patch.labels_remove.is_empty() {
        doc.frontmatter
            .labels
            .retain(|l| !patch.labels_remove.contains(l));
    }
    for add in &patch.labels_add {
        if !doc.frontmatter.labels.contains(add) {
            doc.frontmatter.labels.push(add.clone());
        }
    }
    for (key, value) in &patch.custom {
        if value.is_null() {
            doc.frontmatter.custom_fields.remove(key);
        } else {
            doc.frontmatter
                .custom_fields
                .insert(key.clone(), value.clone());
        }
    }
    if let Some(comment) = &patch.comment {
        // Append the comment to the body so reviewers can see it in the file.
        if !doc.body.is_empty() && !doc.body.ends_with('\n') {
            doc.body.push('\n');
        }
        doc.body.push_str("\n> ");
        for (i, line) in comment.lines().enumerate() {
            if i > 0 {
                doc.body.push_str("\n> ");
            }
            doc.body.push_str(line);
        }
        doc.body.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_native_extracts_kind_and_seq() {
        let (kind, seq) = split_native("TASK-0001").unwrap();
        assert_eq!(kind, "TASK");
        assert_eq!(seq, 1);
    }

    #[test]
    fn split_native_rejects_bad_ids() {
        assert!(split_native("TASK").is_err());
        assert!(split_native("-0001").is_err());
        assert!(split_native("TASK-abc").is_err());
    }
}
