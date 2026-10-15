#!/usr/bin/env node
/** Freeze actual CodeMirror bracket queries over public synthetic Markdown, without a DOM or GUI. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Regeneration is an explicit development operation; normal Rust builds only consume the frozen file.
const args = process.argv.slice(2);
assert.equal(args.length, 3, "Usage: node scripts/generate-bracket-reference.mjs --baseline-root PATH --check|--write");
assert.equal(args[0], "--baseline-root");
const mode = args[2];
assert.ok(mode === "--check" || mode === "--write", "Select an explicit comparison/write mode");
assert.equal(process.versions.node, "24.21.0", "Use the recorded baseline runtime for reproducible grammar behavior");
assert.equal(process.versions.unicode, "17.0", "Unicode properties affect the parser's punctuation classification");
const baselineRoot = resolve(args[1]);
const baselineRequire = createRequire(resolve(baselineRoot, "package.json"));
const { EditorState } = baselineRequire("@codemirror/state");
const { markdown, markdownLanguage } = baselineRequire("@codemirror/lang-markdown");
const { ensureSyntaxTree, matchBrackets, syntaxTree } = baselineRequire("@codemirror/language");
const languagePath = baselineRequire.resolve("@codemirror/language");
const languageSource = readFileSync(languagePath, "utf8");
const sourceStart = languageSource.indexOf("function matchingNodes(");
const sourceEnd = languageSource.indexOf("\n// Counts the column offset", sourceStart);
assert.ok(sourceStart > 0 && sourceEnd > sourceStart, "Expected preserved executable matcher source");
const matcherSource = languageSource.slice(sourceStart, sourceEnd);
const editorSource = readFileSync(resolve(baselineRoot, "src/features/markdown-editor/ui/MarkdownEditor.tsx"), "utf8");
const productConfiguration = editorSource.match(/bracketMatching\(\),\s*markdown\(\{[^]*?\}\),/)?.[0];
assert.equal(productConfiguration?.replace(/\s/g, ""), "bracketMatching(),markdown({base:markdownLanguage,completeHTMLTags:false,}),", "Review changed product bracket/language options before regenerating");

/** Read the real installed package version without relying on package.json export permissions. */
function packageVersion(require_, name) {
  // Package entry points used here live under dist; the manifest remains one directory above it.
  const manifest = JSON.parse(readFileSync(resolve(dirname(require_.resolve(name)), "..", "package.json"), "utf8"));
  assert.equal(manifest.name, name);
  return manifest.version;
}

// These fixtures cover node identity, caret precedence, code/escapes, links, and incomplete delimiters.
const supported = [
  "", "ordinary text", "()", "[]", "{}", "([{}])", "(]", "{a [b} c]", "(a", "a)",
  ")(a)", "(a)(b)", "😀(中文[β])", "𐐀(😀)𐐀", "(a\n\nb)", "(a\r\n\r\nb)",
  "(a *b)* c)", "**(a)** (b)", "*(a* b)", "**(a *b)* c)**", "_(a)_ __[b]__",
  "`(x)` (y)", "(a `)` b)", "``(a ` b)``", "`(a` `b)`", "unclosed `(a)",
  "\\(a\\)", "\\(a) b\\)", "\\\\(a)", "(a\\) b)", "\\[(x)\\]",
  "[label](target)", "before [label](a(b)c) after", "[x](url \"(title)\")", "[x](url '(title)')",
  "[x](url (title))", "[x](<a(b)c>)", "[a(b)]", "[a](bad destination)", "[x][ref]", "[x][]",
  "![a(b)](url)", "[a `]` b](url)", "[x](a\\(b\\)c)", "[x](a(b)c) [y](d(e)f)",
  "[a\\\\](u)", "[a\\]b](u)", "[a\\[b](u)", "[a *b]* c](u)", "[outer ![inner](url)](target)",
  "**(a😀* b)*", "\u00a0*(a)*", "*_(a*) b)_", "((\\[x\\]))", "````\n[x]: (a)\n````",
  "# (a)\n\n## b)", "# (a\n\n# b)", "(a\n===\n\nb)", "a(\n---\n\nb)",
  "```js(info)\n(foo)\n```\n(a)", "```\n(a\n```\n\n```\nb)\n```", "~~~\n[a]\n~~~",
  "    (a)\n\n(b)", "> (a)\n> b)", "- (a)\n- (b)", "- [ ] task (a)", "- [x] task [b]",
  "| (a) | [b] |\n| --- | --- |\n| (c) | (d) |", "~~(a)~~ (b)", "~(a)~ (b)", "^(a)^ (b)",
];
const unsupported = [
  "<div attr=\"(x)\">(y)</div>", "<a href=\"(x)\">(y)</a>", "<!-- (x) --> (y)",
  "<https://example.test/(x)>", "[x][ref]\n\n[ref]: url(x)",
];

// Cross syntax boundaries deterministically, so independently valid fragments cannot hide bad joins.
const fragments = ["(", ")", "[x]", "[", "]", "a", "**b**", "*b*", "`x`", "`(`", "` `", "\\(", "\\)", "\\[", "\\\\", " ", "\n", "\n\n", "😀", "~b~", "^b^", "![x](u)", "[x](u)", "[x][ref]", "{", "}", "**", "_", "~~", "~", "^", "`", "``", "\\", "中", "\t", "# ", "- ", "> ", "\r\n"];
let random = 0x5eed;
for (let index = 0; index < 512; index += 1) {
  // Fixed unsigned arithmetic keeps the corpus reproducible without a random-number package.
  let text = "";
  for (let piece = 0; piece < 10; piece += 1) {
    random = (Math.imul(random, 1_664_525) + 1_013_904_223) >>> 0;
    text += fragments[(random >>> 8) % fragments.length];
  }
  supported.push(text);
}

/** Execute the original matcher with exactly the product's empty-caret/default afterCursor order. */
function query(state, caret) {
  // Each individual call is the actual preserved library function, including token-type resolution.
  return matchBrackets(state, caret, -1)
    || (caret > 0 && matchBrackets(state, caret - 1, 1))
    || matchBrackets(state, caret, 1)
    || (caret < state.doc.length && matchBrackets(state, caret + 1, -1))
    || null;
}

/** Map CodeMirror's normalized line separators back to the unchanged canonical CRLF source. */
function sourceBoundaries(text, normalized) {
  const boundaries = [0];
  let canonical = 0;
  let logical = "";
  while (canonical < text.length) {
    // Default CodeMirror line parsing normalizes CR, LF and CRLF to one LF code unit.
    if (text[canonical] === "\r") {
      canonical += text[canonical + 1] === "\n" ? 2 : 1;
      logical += "\n";
    } else {
      // Keep UTF-16 code-unit boundaries, including surrogate interiors, until caret filtering below.
      logical += text[canonical];
      canonical += 1;
    }
    boundaries.push(canonical);
  }
  assert.equal(logical, normalized, "Unexpected CodeMirror source normalization");
  return boundaries;
}

/** Produce every valid caret query, retaining actual baseline results even for unsupported contexts. */
function recordCase(text, admitted) {
  const state = EditorState.create({ doc: text, extensions: [markdown({ base: markdownLanguage, completeHTMLTags: false })] });
  assert.ok(ensureSyntaxTree(state, state.doc.length, 1_000), "Reference syntax did not complete");
  const boundaries = sourceBoundaries(text, state.doc.toString());
  const queries = [];
  for (let caret = 0; caret <= state.doc.length; caret += 1) {
    // Native carets must be scalar boundaries; the original API accepts UTF-16 interiors but the core rejects them.
    const previous = state.doc.sliceString(caret - 1, caret).charCodeAt(0);
    const next = state.doc.sliceString(caret, caret + 1).charCodeAt(0);
    if (previous >= 0xd800 && previous <= 0xdbff && next >= 0xdc00 && next <= 0xdfff) {
      continue;
    }
    const match = query(state, caret);
    const convert = (range) => ({ start: boundaries[range.from], end: boundaries[range.to] });
    queries.push({ caret: boundaries[caret], expected: match ? { start: convert(match.start), end: match.end ? convert(match.end) : null, matched: match.matched } : null });
  }
  // Escaped backticks have a documented parser-boundary difference; preserve their baseline output
  // while requiring conservative omission instead of admitting a guessed code-span context.
  return { text, admitted: admitted && !text.includes("\\`"), tree: syntaxTree(state).toString(), queries };
}

// Record executable hashes and installed identities; never copy node_modules, private notes or local paths.
const markdownRequire = createRequire(baselineRequire.resolve("@codemirror/lang-markdown"));
const reference = {
  schema: 1,
  provenance: {
    generator: "scripts/generate-bracket-reference.mjs",
    node: process.versions.node,
    unicode: process.versions.unicode,
    language: packageVersion(baselineRequire, "@codemirror/language"),
    state: packageVersion(baselineRequire, "@codemirror/state"),
    markdown: packageVersion(baselineRequire, "@codemirror/lang-markdown"),
    lezer_markdown: packageVersion(markdownRequire, "@lezer/markdown"),
    lezer_html: packageVersion(createRequire(markdownRequire.resolve("@codemirror/lang-html")), "@lezer/html"),
    matcher_sha256: createHash("sha256").update(matcherSource).digest("hex"),
    matcher_file: "@codemirror/language/dist/index.cjs",
    language_entry_sha256: createHash("sha256").update(languageSource).digest("hex"),
    product_configuration_sha256: createHash("sha256").update(productConfiguration).digest("hex"),
    candidate_order: "closing-before, opening-before, opening-after, closing-after",
    configuration: "bracketMatching(); markdown({base:markdownLanguage,completeHTMLTags:false})",
    native_scope: "bounded admitted ASCII handles, not full Lezer/HTML parity",
  },
  cases: [...supported.map((text) => recordCase(text, true)), ...unsupported.map((text) => recordCase(text, false))],
};
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputPath = resolve(repository, "crates/omni-markdown-core/src/markdown_brackets/reference.json");
// One synthetic case per line keeps diff locality without expanding thousands of tiny range objects.
const output = `${JSON.stringify({ schema: reference.schema, provenance: reference.provenance }, null, 2).slice(0, -2)},\n  "cases": [\n${reference.cases.map((item) => `    ${JSON.stringify(item)}`).join(",\n")}\n  ]\n}\n`;
if (mode === "--write") {
  // Only the dedicated fixture is replaced after explicit review of any executable/version drift.
  writeFileSync(outputPath, output);
} else {
  // Byte-for-byte comparison catches changed oracle code, versions, inputs, and expected query outputs.
  assert.equal(readFileSync(outputPath, "utf8"), output, "Bracket reference drift; review before regenerating");
}
console.log(JSON.stringify({ mode, cases: reference.cases.length, queries: reference.cases.reduce((sum, item) => sum + item.queries.length, 0), matcher_sha256: reference.provenance.matcher_sha256 }));
