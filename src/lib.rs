//! Markdown-file subject backend plugin for Animus.
//!
//! This crate is consumed by `src/main.rs` (the stdio plugin binary) and by
//! `tests/contract.rs`. It exposes:
//!
//! - [`config::MarkdownConfig`] — environment-driven configuration
//! - [`backend::MarkdownBackend`] — the `SubjectBackend` implementation
//! - [`parser`] — YAML frontmatter + Markdown body splitter
//! - [`writer`] — deterministic, atomic file writer
//! - [`watcher`] — `notify`-based file watcher fed into `subject/watch`
//! - [`id_gen`] — sequential id generation per subject kind

pub mod backend;
pub mod config;
pub mod id_gen;
pub mod parser;
pub mod watcher;
pub mod writer;
