# Architecture and ownership

The repository separates portable document/workspace authority from the AppKit adapter. The Host
discovers one immutable `REGISTRATION` through the public SDK and mounts a borrowed native-view
descriptor. It does not select Markdown implementation types or manage their internal objects.

| Boundary | Owner and allowed dependencies |
| --- | --- |
| `omni-md-plugin/src/lib.rs` | SDK descriptor, synchronous activation and foreground session; depends on the canonical SDK and this repository's core. |
| `omni-markdown-core` | Piece-tree text/history, streaming syntax queries, confined workspace operations and persistence; contains no AppKit or Host dependency. |
| `native_surface.rs` | Composition imports, shared finite budgets and module assembly; performs no vault mutation itself. |
| `native_surface/controller.rs` | Retained action targets, foreground controls, weak delayed identity and one cancellable autosave timer. |
| `native_surface/deallocation.rs` | Unique-identity public association cohorts and actual native-destruction signals; callbacks retain no tracked graph. |
| `native_surface/lifecycle.rs` | Unique foreground/closing ownership, preflight and weak release barriers; coordinates the SDK lifecycle. |
| `native_surface/window.rs` | Window/session owner, bounded sidebar cursor, current note and staged note/filesystem handoffs. |
| `native_surface/workspace_commands.rs` | Revalidated tree-row intents and modal results translated to typed confined core commands. |
| `native_surface/workspace_actions.rs` | Document actions, cancellable title/body search, native choosers and provisional attachment/text transactions. |
| `native_surface/editor.rs` | One canonical model plus its explicitly retained storage/content/layout/container/view/scroll/ruler graph. |
| `native_surface/text_storage.rs` | Virtual UTF-16 string, immutable derived attributes and balanced canonical edit notifications. |
| `native_surface/text_view.rs` | NSTextInputClient/IME, responder commands and scalar-safe selection reconciliation. |
| `native_surface/table_layout.rs` | Coalesced weak layout work, viewport/table measurements and copied-geometry table painting. |
| `native_surface/table_editor.rs` | One reusable native cell field, commit/cancel and semantic spreadsheet navigation. |
| `native_surface/gutter.rs` and `source_decorations.rs` | Finite source-line/fold metadata and copied display geometry; neither owns a second text representation. |
| `runtime_probe_application.rs` | Feature-gated standalone native event driver shared by independent plugin and pinned Host checks. |

The dependency direction is Host composition → SDK registration → plugin application/native adapter
→ portable core. A native callback queries the canonical document through a weak adapter; it does
not own the canonical document, a hidden note or an alternate mutable string. Source and preview
styles are projections from the same bounded semantic window.

## Foreground lifetime

The SDK session owns the workspace session. The active window surface owns the selected editor,
sidebar cursor and at most one cancellable search worker. The editor constructs and attaches its explicit content/layout/container network before the
text view's designated initializer sees it; it does not replace storage in an implicit native network.
All fallible document acquisition happens before that native allocation. The editor's `NativeMarkdownTextModel`
holds the sole persistent strong handle to the canonical document. Its virtual string and native
storage keep weak document references. AppKit's native relations are retained explicitly where
required and weak where they are reverse/delegate relations.

`foreground_owner.rs` temporarily extracts the unique active surface into a local restoration guard
before a native operation. A Copy-only identity rejects synchronous acquisition during that operation.
The global `RefCell` is unborrowed while AppKit can reenter. Normal return, operation rejection and
unwinding restore the exact owner before clearing that gate. An already accepted handoff whose
native release signal arrives during reentry waits for that exact restoration event; it is neither
discarded as a stale action nor retried by a timer. Extraction does not invalidate the
Host's borrowed view descriptor and does not announce lifecycle completion.

A note/file handoff commits visible table/IME input, saves against the current disk revision,
requires actual responder resignation, seals late input and destroys the old canonical owner. Rust
document/history/snapshot mappings must release on that teardown stack. Native ownership has a
separate completion authority: public Objective-C association markers track unique native object
identities without retaining those objects. Their cohort queues one complete weak-graph check only
after the final tracked deallocation stack has ended. That check must pass before acquiring a
successor. A failed release check preserves the pending transition and exposes its error instead of
reopening a hidden graph.

Window close has separate request, accepted intent, `windowWillClose:`, native-owner destruction,
actual deallocation signal and weak-verification stages. Identical accepted intents coalesce; opposing requests cannot change
the accepted intent. Final verification takes its weak-only evidence out of global state before
native weak loads. Only a passing complete graph check calls the original SDK `finish_close`.
Observed native acceptance status is recorded in [validation](validation.md); these contracts are
requirements, not a claim that every current runtime gate passes. A close that interrupts a pending
note/file handoff must include that earlier editor's weak graph in the same final cohort; an empty
current-editor slot does not prove the preceding graph has deallocated.

The actual signal replaces the earlier guessed next-dispatch authority because an ordinary macOS
27 TextKit control failed that strict boundary while a timestamp-only diagnostic later observed
native deallocation at 511,619 µs. Native reference trees exposed drag-registration hysteresis,
selection/notification and Core Animation paths. Waiting for an actual ownership event preserves
the full nil-graph requirement; it does not accept a surviving view or retain canonical text until
an elapsed deadline. Native handoff may therefore incur measurable system cleanup latency, which
requires performance reporting and comparison. Production uses no cleanup timer or polling. The
standalone probe may use one failure-only deadline to terminate a missing signal, without retrying
or converting a failed graph check to success.

## Text, disk and cancellation

The piece tree owns immutable snapshot/edit pieces and structural undo/redo roots. User-owned files
are never directly memory mapped: source acquisition clones or streams into a distinct private,
unlinked snapshot inode before mapping. External truncate or invalid UTF-8 writes therefore cannot
invalidate live string references. Save streams canonical pieces into a same-directory temporary
file, validates disk revision and atomically replaces only the expected file. A save updates the
saved baseline without discarding structural history or secretly replacing live character content.

An imported attachment remains provisional while canonical insertion is attempted. Successful
checked insertion commits its asset guard; failure explicitly rolls back only the unchanged file
identity acquired by that operation. Detected external replacement or modification is preserved and
reported. Confinement and identity checks are owned by the core, including the documented boundary
against hostile same-user races between separate POSIX calls.

Tree navigation advances a bounded traversal cursor rather than retaining a complete catalog.
Search runs only after explicit submission on one foreground worker with cancellation/generation
checks. Replacing a query, refreshing or closing cancels and joins it. Autosave resets one weak
850 ms one-shot timer only after accepted canonical changes; handoff/close cancels the deadline and
a persistence conflict creates no automatic retry loop. No plugin document, mapping, cursor, timer,
worker or native cache exists in the idle SDK descriptor.
