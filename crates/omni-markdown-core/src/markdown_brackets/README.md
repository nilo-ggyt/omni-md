# Bounded Markdown bracket cues

`markdown_bracket_match(document, caret_utf16)` reads the canonical piece tree and returns
`NoMatch`, `Match`, `BudgetExhausted`, or `ContextUnavailable`. Native code draws only `Match`,
after ending its document borrow and converting the canonical handle ranges to geometry.
The API does not retain an AST, token list, source cache, native object, or second mutable document.

The preserved editor uses `bracketMatching()` and
`markdown({base: markdownLanguage, completeHTMLTags: false})`. Its default candidate order is
closing before the caret, opening before it, opening after it, and closing after it. Plain matching
counts nested delimiters only when their Lezer `NodeType` equals the candidate token's type.
That compares types across distinct nodes: separate paragraphs, escaped pairs, code spans, and
code blocks may each match within their own type. Code content is not universally skipped.
The native implementation keeps that order and nesting behavior for its admitted contexts.

The transient classifier uses existing `pulldown-cmark` offset events plus explicit source checks
for link markers, destinations, titles, reference labels, and escapes. It distinguishes paragraph,
ATX/Setext heading levels, code body/info/span, emphasis/strong/strike/sub/superscript, link/image,
link marker/URL/title/label, table cell, task marker, and escape identities. The reference tests
exercise those contexts against the actual preserved matcher. This evidence does not establish
equivalence between the complete pulldown and Lezer grammars.

There are two separate source bounds:

- A complete document of at most 10,000 UTF-16 units is acquired directly. A larger note uses the
  existing checkpoint-backed complete-block envelope query, whose scan is capped at 128 Ki
  UTF-16 units and 256 KiB UTF-8. Its returned envelope must also fit 10,000 units before parsing
  or allocating bracket-context metadata. A source fallback is never parsed as a fresh document.
- Classification and a directional matching walk accept at most 10,000 UTF-16 units. An earlier
  uncertain candidate suppresses later candidates, so a resource stop cannot silently change caret
  precedence. Reaching an incomplete block edge returns `ContextUnavailable`; only the actual
  document edge can prove a missing partner. `BudgetExhausted` never means EOF or mismatch.

The old CodeMirror limit is nominal rather than a hard character bound: its plain iterator scans
an entire chunk before checking distance, so a giant line may overshoot 10,000. Marked-node
matching does not use that distance limit at all. Native resource behavior is intentionally stricter;
the explicit outcomes expose this difference instead of claiming matching-budget parity.

Unproven contexts omit cues:

- HTML and HTML-like inline source can introduce marked or multi-character handles and nested
  language types. Those are unsupported. The existing block scanner does not track HTML across
  blank lines, so larger documents containing any raw `<` conservatively return
  `ContextUnavailable`. An O(1) OR bit in existing piece metrics follows edits, joins, deletion,
  undo and redo. It is a risk flag, not an HTML parser; even a literal/code `<` can disable these
  large-note cues. Small documents can still match proven code/URL contexts containing `<`.
- Reference definitions are not emitted as offset events, and their label identity is therefore
  unsupported. Shortcut, collapsed and unresolved reference *uses* are covered by the oracle.
- An escaped backtick can expose a parser difference: Lezer rejects an opener immediately after
  another backtick even when that preceding tick was escaped. Any source envelope containing
  backslash-backtick is conservatively omitted instead of trusting pulldown's code-span ranges.
- Giant blocks, partial checkpoint envelopes, invalidated distant checkpoints, search beyond a
  known local envelope, and other unhandled parser events return explicit uncertainty. Editing
  near the beginning of a large note may invalidate the distant checkpoint proof until bounded
  viewport work has rebuilt it; this query never rescans an unbounded prefix on a keystroke.

The fence-length fix accompanying this API preserves `usize` opening lengths in in-memory
block/checkpoint state. The previous `u8` saturation could incorrectly complete a 400-marker
fence at a 300-marker line and corrupt the complete-envelope proof. No persisted format changed.

`reference.json` contains only synthetic inputs. Regenerate or verify it using the preserved
baseline checkout, with no DOM, editor window, AppKit, or user data:

```sh
node scripts/generate-bracket-reference.mjs --baseline-root ../omni-bar --check
node scripts/generate-bracket-reference.mjs --baseline-root ../omni-bar --write
```

The generator executes the real installed `matchBrackets` with the product's Markdown extension,
hashes executable matcher source, records actual package versions, restores CRLF canonical
positions from CodeMirror's normalized lines, and checks every scalar-aligned caret. The frozen
corpus includes explicit supported/unsupported cases and deterministic fragment joins. Unsupported
cases retain the real baseline answers; omitting them does not rewrite the baseline expectation.

Run the reproducible pure query profile separately from native/frame latency gates:

```sh
cargo +1.99.0 test -p omni-markdown-core --features native-markdown-candidate --release \
  --locked --offline profile_large_note_complete_block_queries -- --ignored --nocapture
```

That profile measures source acquisition plus classification/matching after opening/indexing a
7.8 MB synthetic note. It reports the returned classification extent, wall-clock sample distribution,
and canonical storage profile. It does not measure transient peak allocation, native layout,
input delivery, compositor presentation, physical scanout, or perceived latency. No whole-product
functional/performance acceptance follows from this pure profile.

One development run on 2026-10-15 with Rust 1.99.0 release mode, Apple M5 Max, 36 GiB and macOS
27.0.1 opened 7,800,027 synthetic bytes. The classification envelope was 17 UTF-16 units / 17 bytes;
the first post-index query took 0.457625 ms. Across the following 1,000 queries, p50 was 0.308209 ms,
p95 0.332959 ms, p99 0.344917 ms and maximum 0.375250 ms. Canonical storage retained 1,905 pieces,
tree height 12, zero appended/allocated edit bytes, and zero undo/redo entries. These are observations
for this short-block workload, not a worst-case allocation bound or a native latency certification.
