# animus-subject-markdown

Markdown-file subject backend plugin for [Animus](https://github.com/launchapp-dev/animus-cli).

One Markdown file per task. Frontmatter holds the structured fields the
orchestrator needs; the body is freeform. Files live in your repo, version
controlled with everything else, reviewable in a pull request, editable in
any text editor.

> **Why?** Most task systems lock your work items inside a SaaS database that's
> hard to grep, hard to diff, and hard to back up. This backend treats tasks
> like code: tracked in git, owned by the team, portable to any editor or CI
> system. Combine with `animus-subject-sqlite` (private/dispatch-heavy) or
> `animus-subject-linear` (team-of-record) by routing different kinds to
> different backends in your workflow YAML.

## Install

Either install the prebuilt binary from a release tag, or build from source:

```bash
cargo install --git https://github.com/launchapp-dev/animus-subject-markdown --tag v0.1.0
```

Then register it in your project's plugin config and let the Animus daemon
pick it up.

## What it does

| RPC               | Behavior                                                            |
| ----------------- | ------------------------------------------------------------------- |
| `subject/list`    | Walks `<root>/<kind>/*.md`, parses frontmatter, filters in-memory.  |
| `subject/get`     | Reads one `.md` file at the deterministic path for the id.          |
| `subject/update`  | Reads -> merges frontmatter patch -> atomic write back.             |
| `subject/watch`   | `notify`-based file watcher, emits `SubjectChangedEvent` on change. |
| `subject/schema`  | `supports_create=true`, `supports_watch=true`.                      |
| `health/check`    | Verifies the root directory exists and is writable.                 |

## File format

Each subject is one `.md` file under `<root>/<kind>/`. The frontmatter is a
strict YAML block with fields in a fixed order; the body is freeform Markdown.

```markdown
---
id: markdown:TASK-0001
kind: task
title: Fix the login bug
status: in-progress
priority: 3
assignee: alice@example.com
labels:
- auth
- p1
parent_id: null
created_at: 2026-05-18T12:00:00Z
updated_at: 2026-05-18T13:30:00Z
native_status: In Progress
dispatch_label: code-review
status_metadata: {}
attachments: []
custom_fields:
  cycle: 2026-Q2-cycle-1
---
# Fix the login bug

Description goes here. The body is freeform Markdown. Workflow phases
can render it, agents can read it, humans can edit it in their IDE.

## Acceptance criteria
- [ ] Reproduces in staging
- [ ] Fix doesn't break SSO flow
- [ ] Test added

## Notes
Linked to ENG-123 in Linear.
```

### Determinism guarantees

The backend's writer is intentionally boring so that `git diff` on a status
change shows a single-line change, not a reformatting flood:

- Fixed top-level field order: `id`, `kind`, `title`, `status`, `priority`,
  `assignee`, `labels`, `parent_id`, `created_at`, `updated_at`,
  `native_status`, `dispatch_label`, `status_metadata`, `attachments`,
  `custom_fields`.
- `custom_fields` keys are sorted alphabetically (`BTreeMap` under the hood).
- `\n` line endings only — no `\r\n` even on Windows.
- Single trailing newline at end of file.
- 2-space indentation inside YAML maps (`serde_yaml` default).
- `null` is written explicitly for `Option`-valued top-level fields that have
  no value, so adding a value later shows up as a one-line `git diff` rather
  than as a key appearing out of nowhere.

## IDs

Format: `markdown:<KIND>-<SEQ>` where `<SEQ>` is a 4-digit zero-padded
integer. Examples: `markdown:TASK-0001`, `markdown:ISSUE-0042`.

Sequence allocation:

- Scans `<root>/<kind>/` for existing `<KIND>-NNNN.md` files at startup and
  resumes from `max + 1`.
- **Gaps are not filled.** If `TASK-0001` and `TASK-0003` exist, the next
  allocation is `TASK-0004`. This keeps ids monotonic so `git blame` and
  `git log --follow` stay readable.
- The numeric portion can grow past 4 digits (`TASK-12345.md` is valid);
  zero-padding is just the print format up to 9999.

## Configuration (env vars)

| Variable                    | Default                          | Description                                  |
| --------------------------- | -------------------------------- | -------------------------------------------- |
| `ANIMUS_MARKDOWN_ROOT`      | `<cwd>/.animus/subjects`         | Root directory for subject markdown files.   |
| `ANIMUS_MARKDOWN_KINDS`     | `task`                           | Comma-separated subject kinds to read/write. |
| `ANIMUS_MARKDOWN_ID_PREFIX` | `markdown:`                      | Id prefix (use e.g. `tasks:` for nicer ids). |

## Project layout

```
animus-subject-markdown/
├── Cargo.toml
├── plugin.toml          # plugin manifest discovered by the host
├── src/
│   ├── main.rs          # subject_backend_main entrypoint
│   ├── lib.rs
│   ├── backend.rs       # MarkdownBackend impl SubjectBackend
│   ├── parser.rs        # YAML frontmatter + Markdown body splitter
│   ├── writer.rs        # atomic + deterministic file writer
│   ├── watcher.rs       # notify-based file watch
│   ├── id_gen.rs        # sequential id allocator
│   └── config.rs
└── tests/
    ├── contract.rs
    └── fixtures/
```

## Development

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
./target/release/animus-subject-markdown --manifest
```

## License

MIT. See [LICENSE](./LICENSE).
