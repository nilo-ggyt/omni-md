#!/usr/bin/env node
/** Generate frozen word-count compatibility evidence from the preserved product implementation. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// This tool is opt-in development evidence; ordinary Rust tests read the frozen fixture without Node.
const arguments_ = process.argv.slice(2);
assert.equal(arguments_.length, 3, "Usage: node scripts/generate-word-count-reference.mjs --baseline-root PATH --check|--write");
assert.equal(arguments_[0], "--baseline-root", "An explicit preserved baseline checkout is required");
const mode = arguments_[2];
assert.ok(mode === "--check" || mode === "--write", "Choose --check or --write explicitly");
assert.equal(process.versions.node, "24.21.0", "Reference generation requires pinned Node 24.21.0");
assert.equal(process.versions.unicode, "17.0", "The reference engine must use Unicode 17.0");

// Load the original method body, including its actual CodeMirror iterLines traversal and regexp reset.
const baselineRoot = resolve(arguments_[1]);
const baselinePath = resolve(baselineRoot, "src/features/markdown-editor/ui/MarkdownEditor.tsx");
const baselineSource = readFileSync(baselinePath, "utf8");
const method = baselineSource.match(/\bcountWords\(\) \{([\s\S]*?)\n        \},\n        focus\(\)/u);
assert.ok(method, "The original countWords method boundary changed; review extraction before regenerating");
const body = method[1];
const regexp = body.match(/const words = (\/.*?\/[a-z]*);/u);
assert.ok(regexp, "The original word regexp declaration is missing");
assert.equal(regexp[1], "/[\\p{L}\\p{N}]+/gu", "Only the original General_Category=L|N regexp is supported");
const countWords = new Function("editorViewRef", body);
const baselineRequire = createRequire(baselinePath);
const { EditorState } = baselineRequire("@codemirror/state");
// Record the actual preserved line-model version without depending on package.json export permissions.
const statePackagePath = resolve(dirname(baselineRequire.resolve("@codemirror/state")), "..", "package.json");
const statePackage = JSON.parse(readFileSync(statePackagePath, "utf8"));
assert.equal(statePackage.name, "@codemirror/state", "Unexpected baseline line-model package layout");

/** Execute the extracted product method over a real CodeMirror document with its default line parsing. */
function baselineWordCount(text) {
  // Construct the same state.doc shape the product method reads, without creating DOM/editor views.
  const state = EditorState.create({ doc: text });
  const count = countWords({ current: { state } });
  assert.ok(Number.isSafeInteger(count) && count >= 0, "The reference method returned an invalid count");
  return count;
}

// Include category edges that distinguish exact L/N from whitespace, Alphabetic, and emoji segmentation.
const inputs = [
  "", " \t\r\n", "---_.'!?", "😀👨‍👩‍👧‍👦", "alpha-beta", "can't", "snake_case",
  "e\u0301", "e\u0301clair", "éclair", "\u0345", "a\u0345b", "\u05b0",
  "\u0903", "a\u0903b", "\u20dd", "Ⅷ½", "中文", "中文😀中文", "中文123Latin",
  "a\0b", "a\u200db", "a\u00adb", "a\ufeffb", "a\u2028b", "a\u2029b",
  "one\rtwo\nthree\r\nfour", "𐐀𐐨", "1\ufe0f\u20e32", "\ue000", "\u0378",
  "\u{11db0}", "\u{16ea0}", "\u{1e6c0}", "A\u{11db0}9", "--\u{11db0}😀\u{16ea0}--",
];
const cases = inputs.map((text) => {
  // Every expected value comes from the original method; no replacement tokenization supplies an oracle.
  return { text, expected: baselineWordCount(text) };
});

// Hash one explicit membership bit per Unicode code point, with surrogate bits intentionally clear.
const literalEnd = regexp[1].lastIndexOf("/");
const classifier = new RegExp(regexp[1].slice(1, literalEnd), "u");
const membership = new Uint8Array(0x110000 / 8);
let memberCount = 0;
for (let point = 0; point <= 0x10ffff; point += 1) {
  // Rust char has no surrogate values; those non-scalar code points are excluded on both sides.
  if (point >= 0xd800 && point <= 0xdfff) {
    // Preserve the initialized zero bit rather than constructing an invalid Rust-side scalar reference.
    continue;
  }
  if (classifier.test(String.fromCodePoint(point))) {
    // Least-significant-bit-first encoding makes the complete scalar corpus portable across languages.
    membership[point >>> 3] |= 1 << (point & 7);
    memberCount += 1;
  }
}
const classifierHash = createHash("sha256");
classifierHash.update("omni-word-scalar-v1\0Unicode=17.0\0gc=L|N\0");
classifierHash.update(membership);

// All pair/triple joins include empty pieces, combining marks, punctuation, emoji, numbers, and CRLF.
const fragments = [
  "", "A", "7", "中文", "-", "_", "'", "e\u0301", "\u0345", "\u05b0", "\u0903",
  "\u200d", "😀", "Ⅷ", "½", "\u{11db0}", "\u{16ea0}", "\u{1e6c0}", "\r", "\n", "\r\n", "\u2028",
];
const boundaryHash = createHash("sha256");
boundaryHash.update("omni-word-joins-v1\0Unicode=17.0\0gc=L|N\0");
let boundaryCaseCount = 0;
/** Append one exact original-method result in an explicitly fixed-width integer encoding. */
function recordBoundary(text) {
  // Counts fit u32 because each generated input has at most three small fragments.
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(baselineWordCount(text));
  boundaryHash.update(bytes);
  boundaryCaseCount += 1;
}
for (const left of fragments) {
  // Record pairs first in lexical nested-loop order, matching the independent Rust aggregate test.
  for (const right of fragments) {
    // Original regexp traversal sees the concatenated logical text, without awareness of piece boundaries.
    recordBoundary(left + right);
  }
}
for (const left of fragments) {
  // Triples exercise both AVL grouping choices with separators and empty pieces at either boundary.
  for (const middle of fragments) {
    // Keep the middle fragment independent so a combining mark cannot inherit letter membership.
    for (const right of fragments) {
      // Each reference count is computed by the original method, not by a joining formula.
      recordBoundary(left + middle + right);
    }
  }
}

// Persist only public synthetic inputs and content hashes; no local paths, user notes, or dependencies are copied.
const reference = {
  schema: 1,
  provenance: {
    generator: "scripts/generate-word-count-reference.mjs",
    baseline_file: "src/features/markdown-editor/ui/MarkdownEditor.tsx",
    baseline_method_sha256: createHash("sha256").update(body).digest("hex"),
    node: process.versions.node,
    codemirror_state: statePackage.version,
    unicode: process.versions.unicode,
    regexp: regexp[1],
    category: "General_Category=Letter|Number",
    encoding: "membership bits LSB-first by code point; join counts unsigned 32-bit little-endian",
  },
  classifier: { code_points: 0x110000, scalar_values: 0x110000 - 0x800, member_count: memberCount, sha256: classifierHash.digest("hex") },
  boundaries: { fragments, case_count: boundaryCaseCount, sha256: boundaryHash.digest("hex") },
  cases,
};
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputPath = resolve(repository, "crates/omni-markdown-core/src/markdown_document_buffer/queries/word_count_reference.json");
const output = `${JSON.stringify(reference, null, 2)}\n`;
if (mode === "--write") {
  // Regeneration changes only the dedicated fixture and requires an explicit caller-selected mutation mode.
  writeFileSync(outputPath, output);
} else {
  // Check source provenance, category/version, complete classifications, and expected counts byte-for-byte.
  assert.equal(readFileSync(outputPath, "utf8"), output, "Frozen word reference differs; review baseline/data changes before --write");
}
console.log(JSON.stringify({ mode, unicode: reference.provenance.unicode, scalar_values: reference.classifier.scalar_values, members: memberCount, boundary_cases: boundaryCaseCount, literal_cases: cases.length }));
