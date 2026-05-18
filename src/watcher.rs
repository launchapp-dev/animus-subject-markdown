//! `notify`-based filesystem watcher fed into `subject/watch`.
//!
//! The watcher observes the configured root directory (recursively) and
//! converts raw filesystem events into [`SubjectChangedEvent`] values. We only
//! synthesize events for paths that look like managed subject files
//! (`<kind>/<KIND>-NNNN.md`); writes to the staging `*.tmp` file are filtered
//! out so atomic renames don't generate noise.

use std::path::Path;
use std::sync::Arc;

use animus_subject_protocol::{ChangeKind, EventStream, SubjectChangedEvent};
use anyhow::Result;
use futures::stream::StreamExt;
use notify::{
    event::{ModifyKind, RenameMode},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use tokio::sync::mpsc;

use crate::backend::MarkdownBackend;

/// Spawn a filesystem watcher rooted at the backend's subjects directory and
/// return an [`EventStream`] of [`SubjectChangedEvent`]s.
///
/// The watcher runs until the returned stream is dropped. Internally it
/// shuttles `notify` events through a Tokio mpsc channel and resolves each
/// touched path through the backend to produce a fully-formed
/// [`SubjectChangedEvent`].
pub fn spawn_watch(backend: Arc<MarkdownBackend>) -> Result<EventStream> {
    let (tx, rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut raw_watcher: RecommendedWatcher = {
        let tx = tx.clone();
        notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?
    };

    // Watching the root recursively means a fresh `<kind>/` subdir created
    // after startup is picked up automatically.
    let root = backend.config().root.clone();
    if !root.exists() {
        std::fs::create_dir_all(&root)?;
    }
    raw_watcher.watch(&root, RecursiveMode::Recursive)?;

    // We need to keep the watcher alive for as long as the stream is. We move
    // it into the spawned async task that drives event translation.
    let raw_rx = rx;
    let stream = build_stream(backend, raw_watcher, raw_rx);

    Ok(Box::pin(stream))
}

/// Convert the raw `notify` channel into an async [`Stream`] of
/// [`SubjectChangedEvent`]s. The [`RecommendedWatcher`] is owned by the
/// spawned forwarder task so it stays alive for the stream's lifetime.
fn build_stream(
    backend: Arc<MarkdownBackend>,
    watcher: RecommendedWatcher,
    rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
) -> impl futures::Stream<Item = SubjectChangedEvent> + Send {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<SubjectChangedEvent>();
    tokio::spawn(async move {
        // Keep watcher alive for the lifetime of this task.
        let _watcher = watcher;
        let mut rx = rx;
        while let Some(result) = rx.recv().await {
            let event = match result {
                Ok(ev) => ev,
                Err(err) => {
                    tracing::warn!(target: "animus_subject_markdown", ?err, "watch error");
                    continue;
                }
            };
            let change_kind = match classify(&event.kind) {
                Some(k) => k,
                None => continue,
            };
            for path in &event.paths {
                if !is_managed_path(path) {
                    continue;
                }
                if let Some(out) = translate(backend.clone(), path.clone(), change_kind).await {
                    if out_tx.send(out).is_err() {
                        return;
                    }
                }
            }
        }
    });
    tokio_stream::wrappers::UnboundedReceiverStream::new(out_rx)
}

/// Map a raw `notify` [`EventKind`] to a [`ChangeKind`] we care about.
/// Anything else (access, metadata-only, etc.) is ignored.
fn classify(kind: &EventKind) -> Option<ChangeKind> {
    match kind {
        EventKind::Create(_) => Some(ChangeKind::Created),
        EventKind::Modify(ModifyKind::Data(_)) | EventKind::Modify(ModifyKind::Any) => {
            Some(ChangeKind::Updated)
        }
        EventKind::Modify(ModifyKind::Name(rename_mode)) => match rename_mode {
            RenameMode::To => Some(ChangeKind::Created),
            RenameMode::From => Some(ChangeKind::Deleted),
            _ => Some(ChangeKind::Updated),
        },
        EventKind::Remove(_) => Some(ChangeKind::Deleted),
        _ => None,
    }
}

/// A path is "managed" if it ends in `.md` and its directory's name matches
/// one of the configured kinds. Staging `*.tmp` files are ignored.
fn is_managed_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.ends_with(".md") {
        return false;
    }
    // Ignore the atomic-write staging file.
    if name.ends_with(".md.tmp") {
        return false;
    }
    true
}

/// Resolve `path` back through the backend and synthesize a
/// [`SubjectChangedEvent`]. Returns `None` if the file no longer exists or
/// parsing fails.
async fn translate(
    backend: Arc<MarkdownBackend>,
    path: std::path::PathBuf,
    change_kind: ChangeKind,
) -> Option<SubjectChangedEvent> {
    let subject = match backend.read_subject_at(&path).await {
        Ok(subject) => subject,
        Err(err) => {
            tracing::debug!(
                target: "animus_subject_markdown",
                ?err,
                path = %path.display(),
                "failed to parse changed file; skipping"
            );
            return None;
        }
    };
    Some(SubjectChangedEvent {
        id: subject.id.clone(),
        change_kind,
        subject,
        previous_native_status: None,
        previous_dispatch_label: None,
    })
}

// Bring in `tokio_stream` indirectly via futures::Stream trait + an
// unbounded-receiver adapter. We re-implement that adapter inline to avoid
// pulling in the full `tokio-stream` dependency.
mod tokio_stream {
    pub mod wrappers {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use futures::Stream;
        use tokio::sync::mpsc::UnboundedReceiver;

        /// Minimal `Stream` adapter over a Tokio [`UnboundedReceiver`].
        pub struct UnboundedReceiverStream<T> {
            inner: UnboundedReceiver<T>,
        }

        impl<T> UnboundedReceiverStream<T> {
            /// Wrap an [`UnboundedReceiver`] as a [`Stream`].
            pub fn new(inner: UnboundedReceiver<T>) -> Self {
                Self { inner }
            }
        }

        impl<T> Stream for UnboundedReceiverStream<T> {
            type Item = T;
            fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
                self.inner.poll_recv(cx)
            }
        }
    }
}

// Pull the `StreamExt::next` impl into scope at module load time via a
// no-op import, so downstream tests can drive the stream when added.
#[allow(unused_imports)]
use StreamExt as _;
