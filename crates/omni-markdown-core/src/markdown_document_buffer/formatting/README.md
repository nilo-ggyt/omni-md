# Formatting reference corpus

`reference_cases.json` is generated test data, not a runtime text mirror. On 2026-10-15 the
unchanged original `src/features/markdown-editor/model/markdown-formatting.ts` was executed against
its installed CodeMirror State **6.7.1**, using real `EditorState.update` dispatch and selection
mapping. The formatter source SHA-256 is
`e30bf8c8b9933e647d5971bcfb6a8760561228164960581de2e120d5587d3746`.

The 4,180 cases cover all 19 original commands over six synthetic documents: empty input, ordinary
text, existing headings, tabs, a seven-hash nonheading, CJK, emoji, combining marks, a BOM whitespace
prefix, multiple/empty lines and a trailing line break. Selected scalar boundaries include both
ends, early/middle positions, and the next line start; every pair is exercised in both directions.
Each result records actual text and primary UTF-16 anchor/head positions. Rust tests also undo and
redo each case through the document API. Multiple native selections are outside the current
primary-selection API and are not claimed as covered by this corpus.

Independent storage tests cover selections above the viewport materialization cap,
constant backing bytes for at most 4,096 repeated sparse line markers, one-command history, rollback after a closing
marker append succeeds but the opening marker exceeds the edit budget, invalid surrogate
coordinates, the live-document cap, and preservation of existing CRLF/CR bytes. The original
CodeMirror reference normalizes its own document line endings, so the CRLF/CR preservation cases
are an explicit native-storage contract rather than a claim of byte-identical reference input.

The original formatter/build inputs remain in the preserved migration baseline until product
acceptance. The final Rust test suite consumes frozen synthetic expected data and requires no
Node, React, CodeMirror, TypeScript, browser, or network service. Dense selections stream canonical
replacement bytes through two fixed 4 KiB scratch buffers; their full output consumes the normal
session append budget and their new tree metadata grows by output chunk count. Separate tests
cover a million newline-only lines, old edit-backed source while segment storage grows, CRLF split
at a piece boundary, backward ranges and rejection preserving the complete storage profile. Update the corpus only with a
reviewed behavioral requirement and a documented independent reference; do not regenerate expected
results from the Rust implementation being tested.
