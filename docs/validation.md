# Validation record

This record separates source implementation, portable contract checks and actual native acceptance.
The acceptance machine is Apple M5 Max / 36 GiB / arm64 / macOS 27.0.1, using Rust 1.99.0.
macOS 14 remains the deployment floor. Runtime behavior on macOS 14 and M1 / 8 GiB has not been
measured. No CI result or source inspection substitutes for the interactive native gates below.

## Executed portable checks

The core suite covers Unicode/scalar boundaries, persistent piece-tree edits and history, bounded
viewport projection, title/body search and cancellation, relative-path/symlink confinement,
exclusive note/import creation, external-revision conflicts, immutable source snapshots, external
writer subprocess regressions, descriptor/mapping release, and collision-safe draft preservation.
The dated core results and explicit release profiles are maintained in the
[core documentation](../crates/omni-markdown-core/README.md).

The plugin's 18 ordinary tests passed on Rust 1.99.0 after the native module split and foreground-owner
lease integration. They cover immutable SDK registration,
UTF-16 scalar and font-run boundaries, bounded table row selection, spreadsheet navigation,
cell escaping, source table insertion, marker display projection, viewport column sizing,
opposing/duplicate close intents, lease reentry/restoration/unwinding, and control-character classification.
The obsolete plain bracket tests were replaced by the core's actual CodeMirror token-context corpus.
The complete 2026-10-15 source handoff passed workspace formatting, default and probe-feature
all-target Clippy with warnings denied, all-feature rustdoc with warnings denied, 138 core tests
and 18 plugin tests. Six explicitly opt-in core profiling tests were not part of that ordinary run.
The workspace pager additionally verifies that multiple bounded pages reach all fixture notes,
explicit close exhausts cursor ownership, and collapsed/reserved/symlink subtrees do not expose notes.
All-target plugin Clippy with the native probe feature passed with warnings denied.

## Native functionality and acceptance

| Behavior | Implementation and existing evidence | Interactive acceptance |
| --- | --- | --- |
| Native text and source/preview presentation | Actual TextKit 2 surface over the piece tree; Source projects literal syntax with monospaced highlighting, while preview derives rich styles from the same bounded semantic cache. Synthetic Unicode/style assertions passed in the unlocked full probe. | Full canonical event/resource probe passes; physical presentation checks pending. |
| IME and selection | Marked ASCII/Chinese updates, attributed input, emoji/combining marks, actual superclass composition clearing and scalar-safe selection assertions passed in the unlocked probe. | Physical input-source delivery remains pending. |
| Structural undo/redo | Real native responder actions route to the canonical persistent history; unlocked probe assertions passed. | Physical command delivery remains pending; canonical resource gate now passes. |
| Table editing | Bounded visible-cell layout, one reusable field, commit/cancel, Tab/Shift-Tab/Enter and oversized rejected-draft preservation passed the unlocked synthetic assertions. | Physical keyboard/mouse checks pending; canonical resource gate now passes. |
| Vault tree and search | Budgeted metadata pages, folder collapse, explicit refresh and cancellable title/body scan. Core paging/search contracts pass. | Actual controls, later pages and result selection pending. |
| Native file imports | Native `NSOpenPanel`, confined Markdown import, attachment import, file paste/drop and canonical link insertion. Core storage contracts pass. | Actual chooser, paste and drop delivery pending. |
| Conflict recovery | Revision-checked save, confirmed disk reload and exclusively created draft copy preserve external bytes. Core contracts pass. | Actual native conflict dialog and follow-up close pending. |
| Find, replace and document information | Public native finder actions; counts use piece-tree aggregates without flattening text. Word count matches the preserved editor's Unicode 17 Letter/Number runs, including exhaustive scalar and join regressions. | Finder case/wrap/replace behavior and counts display pending. |
| Formatting | Nineteen native menu commands use the atomic core formatter with exact selected ranges, placeholders and one history transaction. Core differential tests cover 4,180 preserved CodeMirror reference cases; native Bold plus single Undo assertions passed in the unlocked probe. | Remaining menu actions and physical keyboard checks pending. |
| Autosave | Canonical edits reset one weak 850 ms nonrepeating timer; handoff/close cancel it and a conflict stops automatic retry. Saving preserves structural undo/redo and selection. | Timer timing, conflict stop/re-edit and immediate-close behavior pending. |
| Source line numbers and folding | A scroll-owned native ruler caches at most 256 visible logical lines; explicit folds retain bounded source intervals and use the public content enumeration delegate. Core fold queries match 317 actual CodeMirror reference queries over 87 sources, including CRLF, long delimiters and opaque/container boundaries. Unlocked native fold roundtrip/source preservation assertions passed and the ruler released. | Physical omission/layout, copy/search, marked text and keyboard/menu accessibility checks pending. |
| Source indentation and contextual editing | Source Tab/Shift-Tab use the atomic two-space core indentation planner; 2,178 actual CodeMirror references cover line endpoint exclusion, tabs and Unicode whitespace. Contextual Enter/Backspace route through the canonical atomic planner, with 4,328 actual CodeMirror references over 117 sources covering declared list/quote/fence/HTML and fallback indentation contexts. This covered corpus does not establish arbitrary embedded-language parser equivalence. | Native key delivery, list continuation and focus traversal pending. |
| Active line, brackets and special characters | Source uses copied native geometry for active-line/ruler highlighting and bounded bracket/control-character decoration. The core classifies admitted token contexts against 588 independent CodeMirror cases at 12,475 scalar carets (546 admitted cases and 42 explicitly uncertain cases) and returns budget/context uncertainty; native geometry is capped at 130 rectangles, including multi-segment handles. | Mixed HTML/reference contexts remain conservatively omitted rather than fully parser-equivalent; visible geometry, control glyph spacing and accessibility pending. |
| Accessibility | Editor, search, document actions and navigation controls expose native labels/help. | VoiceOver focus, announcements and keyboard traversal pending. |
| Close and resource release | Save-before-close, actual responder resignation, sealed late-input gates and weak originating-action identity protect the final saved revision. Canonical document/history/mappings must release synchronously; public association markers signal the actual last tracked native deallocation, then one complete strict nil-graph check must pass before the Host receives completion. | Canonical shared strict graph gate passed after the constructor correction below; unlocked product and latency comparison pending. |

On 2026-10-15, the unlocked full native probe mounted, exercised and closed the synthetic editor
through actual application events. Its input/style/fold/history/table and sealed-late-input
assertions reached the close phase, but final strict weak verification failed. The canonical Rust
document, scroll, ruler, content storage, layout manager and container had released; the native
text view, its virtual storage and its reusable cell remained alive. A native
`NSTextContentStorage` enumeration-during-editing warning was also observed. These are failing
observations; the complete native gate did not pass.

An independent ordinary `NSTextView`/TextKit 2 control also failed its unchanged strict gate on the
unlocked desktop: the ordinary text view survived while its other seven tracked nodes released.
Read-only application-update/run-loop milestones confirmed actual update and before/after-waiting
cycles. One separate diagnostic additionally allowed a real post-close before-waiting boundary;
the same survivor remained, so that scheduling experiment was removed. Read-only allocation and
reverse-reference graphs identified several native retention paths: a Core Animation transaction,
a native drag-registration hysteresis block and a selection-manager/notification association.
Single documented transaction flushes, superclass event dispatch, drag registration update and the
guarded macOS 27 selection-manager nil transition did not make the strict control gate pass; these
experiments were removed.

A separate outside-repository diagnostic preserved the original stage-103 failure and attached a
timestamp-only deallocation marker through the public Objective-C association API. With the actual
application loop continuing naturally, the ordinary view's deallocation signal arrived 511,619 µs
after close, before the diagnostic's one failure-only 2 s deadline. The marker retained no view and
performed no weak retry. This establishes one observed transient native lifetime, not passing
acceptance or a performance distribution. A minimal owner-drop counterfactual also failed its
unchanged eight-node gate. The source lifetime policy now uses that public actual-deallocation event through unique-identity
cohorts, while preserving synchronous canonical release and the complete strict nil graph. No
source change itself constitutes passing verification. The control remains an attribution aid, not a substitute
for the plugin's full test. No waits, retries, skipped assertions or survivor allowances may convert
the existing failure into success.

The first cohort-driven shared probe was executed later on 2026-10-15, while the latest known
desktop state was locked again; no physical presentation or unlocked acceptance is claimed for
that run. It reached the functional close stage but failed its one 2 s missing-signal deadline.
A separate ordinary `NSObject` countertest passed empty-graph completion, duplicate-identity
accounting and actual association-marker destruction without an event-loop delay. A single
terminal failure observation then reported a sealed cohort with exactly one remaining object:
custom text storage was alive, while panel, root view, canonical document, scroll, gutter, text
view, content storage, layout manager, container and reusable cell were all absent. The handler
was installed and unconsumed, with no relay queued. This narrows the remaining native release
issue to the storage's ownership; it does not permit that survivor or classify locked-session
behavior as an unlocked leak. The original queue-boundary failures remain recorded above.

The constructor correction then passed the complete maintained shared native probe on the same
machine. The source now constructs and attaches its owned `NSTextContentStorage`, primary
`NSTextLayoutManager` and `NSTextContainer`, installs the canonical adapter, and passes that exact
network to `NSTextView`'s designated `initWithFrame:textContainer:` initializer. It no longer swaps
storage into an implicitly view-created network or performs fallible graph queries after view
allocation. Controlled outside-repository comparisons established that no-input mount/close passed,
while one ordinary character edit in the old construction reproduced both the warning and storage
survivor; the owned construction passed both that minimal control and the complete unchanged
exercise. Eager attribute fixing, omitted derived attribute notifications, an explicit content
transaction, immutable-string-copy instrumentation and omission of teardown attribute fixing had
not corrected the old graph; none of those experiments were adopted.

The canonical source run reported `canonical_released_synchronously=true`, actual complete native
release at **513,170 µs** and editor release at **504,202 µs**, followed by all tracked weak roles
being absent. No enumeration-during-editing warning occurred. This was a real application-event
and full resource probe with all input/IME/history/table/fold/late-input assertions enabled. The
latest known desktop state was locked again, so it does not establish visible-frame presentation,
physical keyboard delivery or an unlocked product workflow. This one observed fast-open/close
latency is also not a distribution, warmed-user-close result, threshold waiver or baseline
performance comparison. The historical failures above remain valid observations of the earlier
construction and guessed queue boundary.

The Host's refreshed local-path release assembly subsequently executed the same maintained driver,
including the added overlapping-cohort-key countertest. It passed empty/duplicate/overlapping
association accounting, synchronous unmounted document/storage release, the complete functional
exercise and the full nil graph. That run observed native release at **526,315 µs** and editor
release at **505,384 µs**. It used the same SDK identity and the updated Host sources, but remained
a mutable local-path integration with the latest known desktop locked. An InputMethodKit wake-up
port diagnostic was emitted. No physical keyboard, system IME, visible paint or ordinary Host
termination acceptance is inferred from this result.

## Required final checks

Run the documented formatting, Clippy, tests and rustdoc commands, then execute the complete native
probe and ordinary TextKit control on an unlocked interactive session. Verify actual file chooser,
conflict, keyboard/IME, find/replace, attachment and accessibility behavior using isolated fixtures.
Compare resource release and performance against the preserved Host baseline on the same machine.
Host baseline retirement and final product acceptance require those concrete results. The initial
component source publication includes this explicit incomplete acceptance record; publication is
not a product-release or migration-completion claim. This record authorizes no baseline removal
on the strength of portable tests or the shared native driver alone.
