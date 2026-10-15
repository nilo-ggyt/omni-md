# Omni Markdown Core

This crate owns Markdown document state, confined vault operations, and bounded presentation
metadata. It has no AppKit, Host, SDK, WebView, or async-runtime dependency. The plugin's native
adapter owns windows, main-thread callbacks, worker cancellation tokens, and worker join handles.

The `native-markdown-candidate` feature exposes the piece-tree editor and viewport presentation
API. The feature name remains stable for existing consumers while the repository migration is
completed. The default API retains the existing owned-note and recovery-draft contracts.

## Responsibility boundaries

| Module | Responsibility and ownership |
| --- | --- |
| `immutable_snapshot` | Distinct unnamed inode acquisition, APFS clone fast path, fixed-buffer fallback, query-local scratch ownership, and source-independent mapping safety. |
| `markdown_document_buffer` | Active immutable snapshot mapping, append-only edit segments, shared history roots, selection, and bounded viewport access. |
| `markdown_document_buffer::piece_tree` | Immutable AVL leaves and branches, associative metrics, UTF-8/UTF-16/line translation, streamed writes, and revision hashing. |
| `markdown_document_buffer::formatting` | Atomic primary-selection commands and exact TextKit edit receipts; sparse shared markers and dense fixed-scratch canonical replacement. |
| `markdown_document_buffer::editing_commands` | Portable keyboard behavior, selected-line indentation and exact primary-selection receipts. |
| `markdown_document_buffer::command_transaction` | Shared unpublished-root validation, appended-byte rollback and one-step history publication for formatting/keyboard commands. |
| `markdown_document_buffer::block_scan` | Sparse restart checkpoints and conservative complete-block envelopes. Checkpoints retain coordinates and compact state, without source text or an AST. |
| `markdown_document_buffer::queries` | Constant-time live metrics and query-sized literal search across original and edited pieces. |
| `markdown_workspace` | Explicit activation and the public note, directory, attachment, and save authority. |
| `markdown_workspace::paths` | Relative-name validation, canonical root confinement, no-symlink checks, and entry-budgeted traversal. |
| `markdown_workspace::attachments` | Fixed-buffer attachment copy, encoded insertion links, and selective link rewriting after note moves. |
| `markdown_workspace::paging` | Foreground depth-first directory cursors, per-page result/scan budgets, collapse filtering, and deterministic descriptor release. |
| `markdown_workspace::search` | Caller-cancellable bounded vault search, ranking, and compact snippets. |
| `markdown_workspace::storage` | Short-lived mapped inspection, exact revisions, private legacy recovery records, and rollback guards for unpublished files. |
| `markdown_workspace::native_save` | Exclusive streamed save-as-new of live unsaved bytes, including conflicted drafts. |
| `markdown_viewport_presentation` | A generation-bound current-window cache containing style runs and table metadata only. |
| `markdown_viewport_presentation::parser` | CommonMark/GFM event handling, source-marker ownership, table prototypes, and one-pass Unicode coordinate projection. |

Private modules are implementation boundaries. Existing public module paths, public types, and
methods remain available through their established exports.

## Native document contracts

- Original UTF-8 bytes stay in one session-owned immutable snapshot mapping. The mapped inode is
  distinct from the authoritative user file and has no filesystem name or retained descriptor.
  Original and inserted leaves retain
  scalar-aligned ranges. Undo and redo share immutable tree roots; history does not copy complete
  note bodies.
- Editing positions use UTF-16 coordinates and reject the interior of a surrogate pair. Raw
  NSString-compatible code-unit reads deliberately allow individual surrogate units.
- `character_count` reads the root scalar aggregate in constant time and counts CRLF as one
  character. `MarkdownDocumentBuffer::word_count` reads the root aggregate in constant time and
  counts nonempty Unicode 17 letter/number runs, matching the preserved editor's
  `/[\p{L}\p{N}]+/gu` on each CodeMirror line. Punctuation, combining marks, emoji, controls and
  separators break runs. No normalization, locale word segmentation or full-document token list
  is applied; adjacent L/N scalars crossing a piece join still contribute one word.
- `find_utf16` uses query-sized KMP state and streams selected piece ranges. Forward matches start
  at or after the supplied caret; backward matches end at or before it. Wrapping belongs to the
  caller. Case-insensitive mode folds ASCII only and compares other Unicode literally so reported
  UTF-16 ranges preserve source coordinates. Temporary search memory is proportional to the query;
  the method retains no query or document mirror after returning.
- A successful native save streams exact bytes to a same-parent temporary, checks the disk revision,
  atomically replaces the authoritative note, and updates the validated save baseline. Explicit
  save and debounced autosave preserve the canonical mapping, edit segments and bounded undo/redo
  history; all session storage releases at teardown. The saved root controls dirty-state comparison,
  so undoing a save becomes dirty and redoing to that exact saved root becomes clean again.
- Saving does not reset the cumulative **32 MiB session append budget**. The canonical tree and
  bounded history may still reference the original snapshot or older edit segments; saving cannot
  erase them merely to recover space. An exhausted budget rejects the new command without losing
  the current text or history. Closing the session releases all of this storage. This retained
  session budget is separate from live-document size and from bounded viewport scratch memory.
- A native conflict leaves local edits and their history in the live document and preserves the
  external file. `save_document_buffer_as_new` streams those live bytes to a collision-safe exclusive
  destination. It does not mutate or save the original buffer association; the UI switches notes
  only after it accepts the returned receipt.
- The legacy owned-note API retains its private, owner-only conflict-draft format for existing
  consumers. Native document saves do not create those legacy recovery records.
- Vault search checks both the generation and the caller's external cancellation flag. The external
  flag closes the cancel-before-worker-start race. A native session must set its flag, invalidate
  search, and join its own worker before releasing the session.

## Immutable source snapshots

`memmap2` requires callers to prevent mutation of a mapped inode; read-only and `MAP_PRIVATE`
mappings do not freeze files that another process may truncate or rewrite. The core therefore
never maps an authoritative note, external import source, or named recovery record directly.
It first captures a distinct inode and removes its name, then maps only those private immutable
bytes. See the [memmap2 safety contract](https://docs.rs/memmap2/0.9.11/memmap2/struct.MmapOptions.html#safety)
and Apple's [clonefile contract](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/clonefile.2).

On macOS, acquisition attempts `fclonefileat` against the already opened source descriptor. A
same-volume APFS clone shares blocks through copy-on-write while retaining a distinct inode.
Unsupported or cross-volume cloning falls back to positioned reads into an already unlinked
private file through a 64 KiB stack buffer. The fallback adds one read/write pass and temporary
filesystem blocks; neither path allocates a complete Rust heap mirror of the document. System
temporary storage keeps reading a read-only vault independent of permission to create files in
that vault. An active search reuses one empty private directory and directory descriptor across
its body scans; both are released when that query returns. Individual snapshot files remain
unnamed and their descriptors close before immutable mapped bytes are exposed.

The authoritative source descriptor/path identity and content revision remain separate from the
snapshot inode. Before-and-after metadata validation rejects observed source changes rather than
automatically retrying. The fallback is not an atomic filesystem snapshot: concurrent writes may
cause a changed-read or encoding error, but they cannot mutate the private mapped bytes. A native
save still compares its original content revision against a fresh validated source inspection;
conflicts preserve the existing live buffer and external version. Snapshot fallback copying checks
search cancellation between 64 KiB chunks. A synchronous kernel clone/read/write call cannot be
interrupted by that predicate while the kernel is servicing it.

External-writer subprocess regressions truncate and replace source bytes with invalid UTF-8 while
a document or inspection already owns a snapshot. They also stress concurrent search and recovery
reads, verify explicit changed/corrupt outcomes, compare actual descriptor counts in isolated test
processes, and query macOS VM regions to confirm mappings disappear after destruction.

The following warm local observations were collected on 2026-10-15 with Rust 1.99.0 release builds
on the formal Apple M5 Max / 36 GiB / macOS 27.0.1 machine. The profiles ran serially with
`--test-threads=1`; they are measurements of these fixtures rather than cold disk, cross-volume,
or macOS 14/M1 acceptance evidence. The source and temporary volume supported APFS cloning.

| Operation | Samples | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: |
| Standalone immutable snapshot, 1 MiB, clone preferred | 50 | 0.376 | 0.474 | 0.510 |
| Standalone immutable snapshot, 1 MiB, forced streaming fallback | 50 | 0.447 | 0.515 | 0.536 |
| Legacy owned-note open, 1 MiB, including snapshot/hash/metrics/String result | 200 | 3.673 | 3.763 | 4.118 |
| Vault body search, 129 notes / 5 MiB, query-local snapshot namespace | 50 | 46.762 | 48.720 | 51.040 |
| Legacy atomic save, 1 MiB, including source snapshots and durability | 30 | 15.164 | 16.248 | 16.947 |

Run the measurements explicitly with:

```sh
cargo test -p omni-markdown-core --all-features --release profile -- --ignored --nocapture --test-threads=1
```

## Word-count compatibility

The exact predicate is Unicode `General_Category=Letter|Number` (`Lu`, `Ll`, `Lt`, `Lm`, `Lo`,
`Nd`, `Nl`, `No`). It uses pinned
[`unicode-properties` 0.1.4](https://docs.rs/unicode-properties/0.1.4/unicode_properties/), whose
published tables target Unicode 17.0.0, with only its `general-category` feature enabled. The
crate has no allocation or transitive dependency, and no Rust regex engine is added. ASCII takes
an equivalent A-Z/a-z/0-9 path. Rust's `char::is_alphanumeric` is unsuitable here because its
`Alphabetic` classification also includes some combining marks. The Unicode category groups
are defined by [UAX #44](https://www.unicode.org/reports/tr44/#General_Category_Values).

The behavior deliberately retains the original product definition: `alpha-beta` counts two,
`e\u{301}clair` counts two while precomposed `éclair` counts one, contiguous `中文123` counts one,
`Ⅷ½` counts one, and emoji-only or punctuation-only text counts zero. These are regex runs,
not linguistic words or grapheme clusters. Leaf metrics cache run count and first/last membership;
branch joins subtract one only when an L/N run continues across their shared boundary. Edits
recompute affected metrics, and structural undo/redo restore shared cached roots. Querying the
count retains no body, classifier cache or token index.

The frozen [reference fixture](src/markdown_document_buffer/queries/word_count_reference.json)
was generated with Node 24.21.0 / Unicode 17.0 by executing the actual `countWords` method from
`src/features/markdown-editor/ui/MarkdownEditor.tsx` in the preserved Host checkout, over real
CodeMirror documents. Its schema records the method SHA-256, engine/category versions and
CodeMirror package version. It includes 36 literal cases and version/category-tagged SHA-256
references covering all **1,112,064 Unicode scalar values** and **11,132 pair/triple joins**.
Ordinary Rust tests consume this fixture without Node, check all scalar classifications, exercise
all literal-case piece splits and both join groupings, then verify real edits and undo/redo.

The optional development generator requires the preserved Host checkout and its existing
CodeMirror dependency installation. From this repository, using the pinned Node version:

```sh
node scripts/generate-word-count-reference.mjs --baseline-root ../omni-bar --check
```

Use `--write` in place of `--check` only after reviewing an intentional baseline/version change.
The generator refuses another engine Unicode version or regexp category and does not access
user documents, create a DOM/editor view, or add Node to the production build. Its JSON contains
public synthetic strings and hashes, without local absolute paths.

## Verification

Run from the repository root with its pinned stable toolchain:

```sh
cargo fmt --all -- --check
cargo clippy -p omni-markdown-core --all-targets --no-default-features -- -D warnings
cargo test -p omni-markdown-core --no-default-features
cargo clippy -p omni-markdown-core --all-targets --all-features -- -D warnings
cargo test -p omni-markdown-core --all-features
RUSTDOCFLAGS='-D warnings' cargo doc -p omni-markdown-core --all-features --no-deps
```

The ordinary suite includes differential Unicode editing, AVL and aggregate invariants, shared
history, viewport bounds, table/header checkpoint recovery, confinement, external conflicts,
exclusive draft-copy preservation, history-preserving save/redo, post-rename directory-sync failure
and retry, 4,180 frozen original-formatter/CodeMirror differential cases, dense/sparse formatting
resource bounds, and cancellation-before-start. The ignored storage and snapshot profiles are opt-in release measurements; ordinary test runs do
not execute them. The dated measurements above record the separate explicit profile invocation. Native AppKit probes and acceptance-machine performance measurements belong
to the plugin repository's separate integration gates.

### Source folding

`markdown_folding` resolves explicit heading-section and multiline-block fold ranges in canonical
UTF-16 coordinates. Its private `lines` module streams source with fixed-size scratch and retains
finite line prefixes; `delimiters` computes exact full-line fence/Setext/thematic metadata and finite
HTML termination state. No full-document parser tree or text mirror is created. Cheap viewport hints
inspect at most three fixed prefixes; only the complete explicit query authorizes hiding. Fold state
belongs to the native view and is discarded after any canonical text generation change.

The folding reference corpus contains **317 actual CodeMirror line queries across 87 source inputs**
generated from the preserved product's `@codemirror/lang-markdown` implementation with
`markdownLanguage` (GFM). The original 226 rows are retained. Added cases cover complete giant fence
runs and invalid info, lazy quote/list boundaries, nested list headings, multiline and long Setext
underlines, ASCII/control/Unicode whitespace distinctions, raw HTML case and exact tag-name boundaries.
CRLF fixtures and a derived LF-to-CRLF property translate normalized editor coordinates back to exact
stored UTF-16 offsets. This corpus establishes the covered cases; the finite block scanner is not an
exhaustive Markdown grammar proof. An uncertain long custom HTML start grants no hiding authority and
remains opaque until a blank separator. Recognizable raw tags/comments keep their explicit terminator,
including across blank lines; truncating a prefix never silently forgets a known fence or raw context.

Additional regressions cover a 1 MiB Unicode line, 2,048 subordinate headings, canonical piece edits,
CRLF/surrogate boundaries across scratch chunks, 131,072-line prefixes, distant fence-tail/info text,
and giant hint lines. Queries leave canonical text, generation, history and edit-storage unchanged.
An explicit query performs source-linear context/section work with bounded scratch and indexed piece
access; paint-time hints do not perform that complete scan.

A separate 32-sample Rust 1.99.0 release profile on the acceptance machine measured the million-line
prefix query and 4 MiB fence hint/explicit query. The test prints p50/p95/p99 and unchanged canonical
storage profiles; run `cargo test -p omni-markdown-core --release --features native-markdown-candidate
profile_dense_prefix_and_giant_fence_queries -- --ignored --nocapture` to refresh those measurements.
This pure profile does not validate native layout, native object release or macOS 14 runtime behavior.
