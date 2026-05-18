//! Environment-driven configuration for the Markdown subject backend.
//!
//! All fields are populated from environment variables so the plugin can be
//! launched as a stdio child process without command-line argument plumbing.

use std::path::PathBuf;

use anyhow::Result;

/// Environment variable holding the root directory for subject markdown files.
///
/// Default: `<cwd>/.animus/subjects`. The plugin resolves the root once at
/// startup and treats it as the canonical location; relative paths are joined
/// against the launching process's working directory.
pub const ENV_ROOT: &str = "ANIMUS_MARKDOWN_ROOT";

/// Environment variable holding the comma-separated subject kinds this
/// backend produces. Default: `"task"`.
pub const ENV_KINDS: &str = "ANIMUS_MARKDOWN_KINDS";

/// Environment variable holding the `SubjectId` prefix. Default: `"markdown:"`.
pub const ENV_ID_PREFIX: &str = "ANIMUS_MARKDOWN_ID_PREFIX";

/// Default root directory relative to the launching process's cwd.
pub const DEFAULT_ROOT_REL: &str = ".animus/subjects";

/// Default subject kind emitted when `ANIMUS_MARKDOWN_KINDS` is unset.
pub const DEFAULT_KIND: &str = "task";

/// Default id prefix used by [`SubjectId`](animus_subject_protocol::SubjectId).
pub const DEFAULT_ID_PREFIX: &str = "markdown:";

/// Runtime configuration for the Markdown backend plugin.
#[derive(Debug, Clone)]
pub struct MarkdownConfig {
    /// Absolute path to the subjects root directory.
    pub root: PathBuf,
    /// Subject kinds this backend reads/writes. Each kind corresponds to a
    /// subdirectory under [`Self::root`].
    pub kinds: Vec<String>,
    /// Prefix used on [`SubjectId`](animus_subject_protocol::SubjectId) values.
    pub id_prefix: String,
}

impl MarkdownConfig {
    /// Read the configuration from environment variables.
    ///
    /// Lenient: every field has a default, so the plugin always builds
    /// successfully. The root directory is NOT created here — that happens on
    /// the first write or [`crate::backend::MarkdownBackend::health`] call.
    pub fn from_env() -> Result<Self> {
        let root = std::env::var(ENV_ROOT)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(DEFAULT_ROOT_REL)
            });

        let kinds = std::env::var(ENV_KINDS)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec![DEFAULT_KIND.to_string()]);

        let id_prefix = std::env::var(ENV_ID_PREFIX)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ID_PREFIX.to_string());

        Ok(Self {
            root,
            kinds,
            id_prefix,
        })
    }

    /// Construct a config in-memory (used by tests).
    pub fn new(root: impl Into<PathBuf>, kinds: Vec<String>, id_prefix: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            kinds,
            id_prefix: id_prefix.into(),
        }
    }

    /// Path to the directory holding subjects of `kind`.
    pub fn kind_dir(&self, kind: &str) -> PathBuf {
        self.root.join(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_builds_config() {
        let tmp = TempDir::new().unwrap();
        let cfg = MarkdownConfig::new(tmp.path(), vec!["task".into()], "markdown:");
        assert_eq!(cfg.root, tmp.path());
        assert_eq!(cfg.kinds, vec!["task".to_string()]);
        assert_eq!(cfg.id_prefix, "markdown:");
        assert_eq!(cfg.kind_dir("task"), tmp.path().join("task"));
    }
}
