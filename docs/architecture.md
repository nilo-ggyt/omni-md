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
| `native_input_trace` and `native_surface/input_trace.rs` | Optional fixed-capacity scalar provenance and explicit arm/drain controls; absent from default builds, no text/native owners or output. |
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
native weak loads. For `ForegroundTransition`, only a passing complete graph check calls the original SDK `finish_close`.
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

## Application termination scope

SDK package 2.0.0 / protocol 2 carries an explicit `PluginCloseRequest { intent, scope }` through
`request_close`, `begin_close` and `finish_close`. Ordinary close and all note/file handoffs retain
complete canonical/native nil-graph authority. Terminal scope is a separate process-exit transaction;
it never creates a successor, negotiates protocol versions or adds a runtime compatibility path.

The distinction follows production-shaped synthetic evidence. `NSTerminateLater` waits inside an
AppKit caller pool. Before a local Host save/resign/close preflight pool was added, native temporary
retains kept the editor subtree alive. Draining that effects pool while the Host remains Preparing
released every editor role at approximately 502 ms, but a memory graph still showed four outstanding
panel autoreleases in AppKit's caller pool. The earlier strict terminal tests failed; that evidence
is preserved and is not relabelled as complete window deallocation. See the Host's
[termination decision](https://github.com/nilo-ggyt/omni-bar/blob/main/docs/architecture/application-termination.md)
and Apple's [termination reply contract](https://developer.apple.com/documentation/appkit/nsapplication/terminatereply/terminatelater).

Final close registers two independently keyed, deduplicated cohorts: the original complete graph,
and a resource graph containing every controller/content/editor/TextKit/gutter/cell identity,
including predecessor editors from interrupted handoffs. Canonical/history/mapping ownership and
foreground workers still end synchronously. An ordinary completion consumes only complete-graph
release. A terminal completion requires the resource cohort's actual last deallocation event and
one strict full payload nil check. It then transiently upgrades the panel inside a bounded local
autorelease pool and verifies the real close notification, invisible/nonkey state, nil content,
delegate and initial responder, no payload responder or sheet, and ended project strong ownership.
The custom `NativeMarkdownPanel` subclass has no Rust ivars or retained project payload; its
overrides are static routing methods, not a second controller. The copied receipt can report an
empty framework lease of that subclass; it does not claim base-NSPanel identity or panel deallocation.
A scalar RAII close-call depth must also be zero, so a native nested loop cannot misclassify a
still-local project panel clone as framework ownership. A failed check is not retried.

Before emitting that terminal receipt, full-graph observation is cancelled atomically and its
callback captures are removed. Only then does public Objective-C association removal detach that
cohort's own panel marker. Cancellation can never satisfy `has_completed`; no project callback or
marker metadata remains attached to the framework shell. The transient upgrade and local pool
end before the Host receives `finish_close` with the exact accepted terminal request.

An already accepted ordinary close may promote only its scope after successful Host `begin_close`
authorization for actual pending Quit. Intent and generation stay unchanged; there is no second
composition/save/resign/close operation. A real resource event arriving before promotion is stored
as scalar readiness; successful promotion reserves its single terminal check. Promotion arriving
first uses the existing resource callback. Neither order polls or retries. Reentrant promotion
while native verification is local rejects Busy without effects, leaving the old ordinary close
intact; an already committed identical terminal request still coalesces. Failed proofs preserve
Closing and never retry, downgrade or emit a successful receipt. Host must independently drop its
SDK session, consume its original barrier and authorize only the matching process termination.

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


Autosave cancellation takes its sole project owner and calls `invalidate` on the same AppKit
installation thread. Generation and weak-controller guards revoke stale callback authority.
Apple's [invalidation contract](https://developer.apple.com/documentation/foundation/timer/invalidate())
guarantees the timer becomes invalid and never fires again, while permitting the run loop to release
its strong timer reference later. This is a stopped capability, not a claim that every historical
invalidated timer shell has deallocated. Controller/payload identities still require actual release;
no timer-object survivor is used to relax those strict native widget checks.
