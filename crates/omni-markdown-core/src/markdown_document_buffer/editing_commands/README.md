# Keyboard command reference corpus

`reference_cases.json` contains 2,178 headless executions of the preserved product's actual
`@codemirror/commands` 6.11.0 `indentMore` and `indentLess`, using `@codemirror/state` 6.7.1.
The executable command module SHA-256 is
`6954654363ad59d89a9264f4c402f6667720665fe3bb6c9966a210947c14c164`.

Regenerate with `node scripts/generate-editing-reference.mjs BASELINE_DIRECTORY`. The baseline
directory must contain its original `package.json` and installed lockfile dependencies; the script
uses those actual commands rather than translating their implementation. Fixtures include empty
documents/lines, both selection directions, prefix interiors, trailing line-start endpoints, tabs,
ECMAScript Unicode whitespace, emoji/combining marks, and configured LF/CRLF/CR separators.

CodeMirror represents a configured line separator as one logical document unit. The generator
translates both initial and resulting positions to the original physical UTF-16 coordinates used
by the native piece tree, while preserving exact persisted separator bytes. Cargo tests consume
the frozen synthetic data and do not require Node, a WebView or the former frontend.

Native command behavior still requires an unlocked AppKit run. These pure comparisons establish
text/primary-selection semantics and atomic history only; they do not prove IME, key routing,
rendering performance or graph release.
