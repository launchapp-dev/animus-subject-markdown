//! Sequential id generation for Markdown-backed subjects.
//!
//! Ids look like `markdown:TASK-0001` (4-digit zero-padded sequence number,
//! kind uppercased). The next sequence number is the maximum existing
//! sequence for that kind plus one — we do NOT fill gaps. If `TASK-0001` and
//! `TASK-0003` exist, the next allocation is `TASK-0004`. This keeps ids
//! monotonic, which makes `git blame` and `git log` easy to reason about.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

/// In-memory sequential generator for one subject kind.
///
/// The generator is initialized from the on-disk filesystem (the maximum
/// existing sequence number) once, then advances purely in memory. Concurrent
/// callers serialize through the inner [`AtomicU32`].
#[derive(Debug)]
pub struct SequenceGenerator {
    next: AtomicU32,
}

impl SequenceGenerator {
    /// Build a generator whose first emitted sequence is `start`.
    pub fn starting_at(start: u32) -> Self {
        Self {
            next: AtomicU32::new(start),
        }
    }

    /// Allocate and return the next sequence number.
    pub fn next(&self) -> u32 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }

    /// Scan `kind_dir` for existing `<KIND>-NNNN.md` files and return a
    /// generator whose first emitted sequence is `max_existing + 1`. If the
    /// directory does not exist or holds no matching files, the generator
    /// starts at `1`.
    pub fn discover(kind_dir: &Path, kind_upper: &str) -> Self {
        let max = scan_max_sequence(kind_dir, kind_upper);
        Self::starting_at(max.map(|n| n + 1).unwrap_or(1))
    }
}

/// Scan `dir` for files named `<kind_upper>-NNNN.md` and return the largest
/// `NNNN` found, or `None` if there are no matches.
pub fn scan_max_sequence(dir: &Path, kind_upper: &str) -> Option<u32> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<u32> = None;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if let Some(seq) = parse_sequence(&name, kind_upper) {
            best = Some(best.map_or(seq, |b| b.max(seq)));
        }
    }
    best
}

/// Parse a sequence number out of a file name shaped like `<kind_upper>-NNNN.md`.
///
/// `kind_upper` is matched case-sensitively. `NNNN` is parsed as a decimal
/// integer; widths other than 4 are still accepted so out-of-band edits don't
/// trip the parser (`TASK-12345.md` parses to `12345`).
pub fn parse_sequence(file_name: &str, kind_upper: &str) -> Option<u32> {
    let stem = file_name.strip_suffix(".md")?;
    let rest = stem.strip_prefix(kind_upper)?;
    let seq = rest.strip_prefix('-')?;
    if seq.is_empty() {
        return None;
    }
    seq.parse::<u32>().ok()
}

/// Format the on-disk file name for one subject.
pub fn file_name_for(kind_upper: &str, seq: u32) -> String {
    format!("{kind_upper}-{seq:04}.md")
}

/// Format the native portion of a [`SubjectId`](animus_subject_protocol::SubjectId)
/// (the part after the configurable prefix). e.g. `("TASK", 7)` -> `"TASK-0007"`.
pub fn native_id(kind_upper: &str, seq: u32) -> String {
    format!("{kind_upper}-{seq:04}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_sequence_extracts_number() {
        assert_eq!(parse_sequence("TASK-0001.md", "TASK"), Some(1));
        assert_eq!(parse_sequence("TASK-0042.md", "TASK"), Some(42));
        assert_eq!(parse_sequence("TASK-12345.md", "TASK"), Some(12345));
    }

    #[test]
    fn parse_sequence_rejects_mismatched_kind() {
        assert_eq!(parse_sequence("ISSUE-0001.md", "TASK"), None);
        assert_eq!(parse_sequence("README.md", "TASK"), None);
        assert_eq!(parse_sequence("TASK-.md", "TASK"), None);
        assert_eq!(parse_sequence("TASK-0001.txt", "TASK"), None);
    }

    #[test]
    fn discover_starts_at_one_for_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let gen = SequenceGenerator::discover(tmp.path(), "TASK");
        assert_eq!(gen.next(), 1);
        assert_eq!(gen.next(), 2);
    }

    #[test]
    fn discover_starts_after_max_existing_and_skips_gaps() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("TASK-0001.md"), "").unwrap();
        std::fs::write(tmp.path().join("TASK-0003.md"), "").unwrap();
        let gen = SequenceGenerator::discover(tmp.path(), "TASK");
        // Existing max is 3, so next is 4 — gap at 0002 is NOT filled.
        assert_eq!(gen.next(), 4);
    }

    #[test]
    fn discover_ignores_other_kinds_and_non_md_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("ISSUE-0010.md"), "").unwrap();
        std::fs::write(tmp.path().join("TASK-0002.md"), "").unwrap();
        std::fs::write(tmp.path().join("TASK-0002.bak"), "").unwrap();
        let gen = SequenceGenerator::discover(tmp.path(), "TASK");
        assert_eq!(gen.next(), 3);
    }

    #[test]
    fn file_name_and_native_id_are_zero_padded() {
        assert_eq!(file_name_for("TASK", 7), "TASK-0007.md");
        assert_eq!(native_id("TASK", 7), "TASK-0007");
        assert_eq!(file_name_for("TASK", 12345), "TASK-12345.md");
    }
}
