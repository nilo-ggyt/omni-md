// Generate portable Markdown keyboard fixtures by executing the preserved product's real commands.
// The baseline is an explicit reviewer-supplied checkout; this optional tool installs no packages.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const baseline = process.argv[2];
const options = process.argv.slice(3);
if (!baseline || options.some(option => option !== '--check')) {
  // Refuse implicit global resolution or unknown options that could select a different oracle.
  throw new Error('usage: node scripts/generate-markup-reference.mjs BASELINE_DIRECTORY [--check]');
}
const check = options.includes('--check');
const baselineRequire = createRequire(path.resolve(baseline, 'package.json'));
const markdownRequire = createRequire(baselineRequire.resolve('@codemirror/lang-markdown'));
const htmlRequire = createRequire(markdownRequire.resolve('@codemirror/lang-html'));
const javascriptRequire = createRequire(htmlRequire.resolve('@codemirror/lang-javascript'));
const cssRequire = createRequire(htmlRequire.resolve('@codemirror/lang-css'));

// Pin executable modules involved in Markdown, default command and embedded HTML/JS/CSS behavior.
const expectedProvenance = {
  "@codemirror/state": {
    "version": "6.7.1",
    "sha256": "59360309804102f5af5b5a9a329ffe6d1ee370a38c4cc571047686e0bd621e09"
  },
  "@codemirror/commands": {
    "version": "6.11.0",
    "sha256": "6954654363ad59d89a9264f4c402f6667720665fe3bb6c9966a210947c14c164"
  },
  "@codemirror/language": {
    "version": "6.12.4",
    "sha256": "34db623cd5be393d2c6e42a8136882a8d4fa36438091bb44093426e08ed0b7f0"
  },
  "@codemirror/lang-markdown": {
    "version": "6.5.2",
    "sha256": "fbef0a4eb11d458a47783da78b9fa248807c02040d53f891f087ecd82d9d08a7"
  },
  "@lezer/markdown": {
    "version": "1.7.2",
    "sha256": "72d79eb50be18e86856a7f630c1af49473e6eb6d434fdf2a39379fc82223932f"
  },
  "@codemirror/lang-html": {
    "version": "6.4.12",
    "sha256": "08b1f5d75549fdf692589185a31490cced0bc3a7b0fbbea0aa8d7b1ea2fcd960"
  },
  "@lezer/html": {
    "version": "1.3.13",
    "sha256": "a56a1b382e1b337a82bc215aadab8f9d1cedc4df125c8f1071a538e12cbc0cfd"
  },
  "@codemirror/lang-javascript": {
    "version": "6.2.5",
    "sha256": "85086af4aa0f197d88080d2a72a7e16b7c056d3353a834b6f49589c1059bd477"
  },
  "@lezer/javascript": {
    "version": "1.5.4",
    "sha256": "e2adde9155790ddb888db4b97c7deba2de3e3e9a71f8289f483c149c910a86f3"
  },
  "@codemirror/lang-css": {
    "version": "6.3.1",
    "sha256": "3188b8677b8ddb4f3e3f0efbe1b2a59f527c19569ba15b6bd25e8d238ca88a4c"
  },
  "@lezer/css": {
    "version": "1.3.6",
    "sha256": "d0348c923acf6f02fb6fd9dbd103664cbc6491ff2dfe400ab5612cf5c7d64081"
  }
};
const sources = [
  ['@codemirror/state', baselineRequire],
  ['@codemirror/commands', baselineRequire],
  ['@codemirror/language', baselineRequire],
  ['@codemirror/lang-markdown', baselineRequire],
  ['@lezer/markdown', markdownRequire],
  ['@codemirror/lang-html', markdownRequire],
  ['@lezer/html', htmlRequire],
  ['@codemirror/lang-javascript', htmlRequire],
  ['@lezer/javascript', javascriptRequire],
  ['@codemirror/lang-css', htmlRequire],
  ['@lezer/css', cssRequire],
];
const provenance = {};
for (const [name, resolveFrom] of sources) {
  // Resolve each transitive module from its actual owner, preserving pnpm/nonhoisted package identity.
  const source = resolveFrom.resolve(name);
  const version = JSON.parse(fs.readFileSync(path.resolve(source, '../../package.json'), 'utf8')).version;
  const sha256 = crypto.createHash('sha256').update(fs.readFileSync(source)).digest('hex');
  provenance[name] = { version, sha256 };
  assert.deepEqual(provenance[name], expectedProvenance[name], `Oracle executable drift: ${name}`);
}
const { EditorState, EditorSelection } = baselineRequire('@codemirror/state');
const { insertNewlineAndIndent } = baselineRequire('@codemirror/commands');
const { indentUnit, ensureSyntaxTree } = baselineRequire('@codemirror/language');
const { markdown, markdownLanguage, insertNewlineContinueMarkup, deleteMarkupBackward } = baselineRequire('@codemirror/lang-markdown');

// Inputs cover syntax ownership, marker affinity, list structure, opaque regions and native coordinates.
const inputs = [
  "",
  "plain text",
  "# Heading",
  "  plain",
  "    code",
  "   ",
  "\t\t",
  "a   b",
  "a\u00a0\u2003b",
  "a()b",
  "[]",
  "{}",
  "( )",
  "- item",
  "- ",
  "-  ",
  "+ item",
  "* item",
  "- first\n- second\n- third",
  "- first\n- ",
  "- first\n- second\n- ",
  "- first\n\n- ",
  "- first\n\n- second\n\n- third",
  "- first\n  continued",
  "- first\ncontinued",
  "- first\n\n  continued",
  "- first\n  - nested",
  "- first\n  - ",
  "- first\n  - nested\n  - ",
  "- first\n  > quote",
  "- > - item",
  "1. first",
  "1. ",
  "1. first\n2. second\n3. third",
  "9. first\n10. second\n11. third",
  "01. first\n02. second\n03. third",
  "1) first\n2) second\n3) third",
  "1. first\n3. third\n4. fourth",
  "1. first\n2. ",
  "1. first\n2. second\n3. ",
  "1. first\n\n2. second\n\n3. third",
  "- first\n  1. nested\n  2. child\n- second",
  "1. first\n   1. child\n   2. ",
  "> quote",
  "> ",
  ">\n>",
  "> \n> ",
  "> first\n> \n> ",
  "> first\ncontinued",
  ">> nested",
  "> > nested",
  "> > ",
  "> - item",
  "> - ",
  "> - first\n> - ",
  "- [ ] task",
  "- [x] done",
  "- [X] done",
  "- [ ] ",
  "-  [X]  done",
  "- first\n- [x] second\n- [ ] third",
  "```\n- code\n```",
  "```rust\n1. code\n```",
  "- ```\n  code\n  ```",
  "> ```\n> - code\n> ```",
  "~~~\n> code\n~~~",
  "    - code",
  "<script>\n- code\n</script>",
  "<!--\n- code\n-->",
  "-\titem",
  "-\titem\n    - child",
  "-\titem\n\t- child",
  "1.\titem\n\t- child",
  "1.\titem\n    1. child\n    2. child",
  "1.\titem",
  ">\tquote",
  "  > quote",
  "   - item",
  "-     code",
  "1.     code",
  "- \u4e2d\u6587\ud83d\ude00 e\u0301",
  "> \u4e2d\u6587\ud83d\ude00\n> \ud801\udc00",
  "- one\r\n- two\r\n- three",
  "> one\r> two",
  "first\n\n- item\n  continuation\n\nparagraph",
  "---\nbody",
  "Title\n===\nbody",
  "- `code`\ncontinued",
  "- #not heading\ncontinued",
  "- # Heading\noutside",
  "- ***\noutside",
  "paragraph\n- ",
  "paragraph\n2. item",
  "- first\n2. text",
  "- first\n\t- nested",
  "> first\n> \t- nested",
  "- first\n\tcontinued",
  "- first\n\n\t- nested",
  "1. first\n   - second\n2. outer",
  "- - -",
  "* * *",
  "___",
  "<div>\n- literal\n</div>",
  "<b>word</b>",
  "<div><span>text</span></div>",
  "hello <b>bold</b>",
  "<pre>\n- text\n</pre>",
  "<!--\n\n- text\n-->",
  "- <script>\n  - text\n- outside",
  "- ```\n  code\n- outside",
  "> ```\n> code\noutside",
  "```\n<script>\n- text\n</script>\n```",
  "> \r\n> ",
  "1. first\r\n2. \r\n3. third",
  "- first\n  - nested\n- \n- third",
  "- first\n\n- second\n- third",
  "1. first\n2. second\n\n3. third"
];
const cases = [];
for (const input of inputs) {
  // A preserved editor state has one physical separator; serialize endpoints back into native raw units.
  const separator = input.includes('\r\n') ? '\r\n' : input.includes('\r') ? '\r' : '\n';
  const base = EditorState.create({
    doc: input,
    extensions: [EditorState.lineSeparator.of(separator), indentUnit.of('  '), markdown({ base: markdownLanguage })],
  });
  const logical = base.doc.toString();
  const boundaries = [0];
  for (const scalar of logical) {
    // Primary selections never enter the middle of an emoji/supplementary surrogate pair.
    boundaries.push(boundaries.at(-1) + scalar.length);
  }
  const selections = boundaries.map(position => [position, position]);
  for (const [anchor, head] of [[0, logical.length], [logical.length, 0], [1, Math.max(1, logical.length - 1)]]) {
    // Include reversed/multiline selections so Markdown decline/default Enter association is independently frozen.
    if (anchor !== head && boundaries.includes(anchor) && boundaries.includes(head)) {
      // Scalar-aligned nonempty ranges exercise default replacement rather than an invalid input artifact.
      selections.push([anchor, head]);
    }
  }
  for (const [action, command] of [['newline', insertNewlineContinueMarkup], ['backspace', deleteMarkupBackward]]) {
    // Call the package's exported command directly, with no copied/reimplemented JavaScript algorithm.
    for (const [anchor, head] of selections) {
      // Every query owns a fresh selection/state; syntax completion prevents timeout-dependent partial parses.
      let state = base.update({ selection: EditorSelection.single(anchor, head) }).state;
      assert.ok(ensureSyntaxTree(state, state.doc.length, 1000), 'Incomplete oracle syntax tree');
      const before = state;
      const dispatch = transaction => {
        // Actual command transactions publish their authoritative text and selection into the oracle state.
        state = transaction.state;
      };
      const markupHandled = command({ state, dispatch });
      let handled = markupHandled;
      if (!handled && action === 'newline') {
        // The preserved default keymap invokes its ordinary newline/indent command after Markdown declines.
        handled = insertNewlineAndIndent({ state, dispatch });
      }
      cases.push({
        action, input, markup_handled: markupHandled, handled,
        anchor: before.sliceDoc(0, anchor).length,
        head: before.sliceDoc(0, head).length,
        text: state.sliceDoc(),
        anchor_after: state.sliceDoc(0, state.selection.main.anchor).length,
        head_after: state.sliceDoc(0, state.selection.main.head).length,
      });
    }
  }
}
const destination = fileURLToPath(new URL('../crates/omni-markdown-core/src/markdown_document_buffer/editing_commands/markup/reference.json', import.meta.url));
const output = JSON.stringify({ provenance, cases }) + '\n';
if (check) {
  // Verification is read-only and fails for any source/selection/result/provenance drift.
  assert.equal(fs.readFileSync(destination, 'utf8'), output, 'Markup reference corpus drift');
} else {
  // Explicit regeneration replaces only the frozen synthetic fixture artifact.
  fs.writeFileSync(destination, output);
}
process.stdout.write(JSON.stringify({ mode: check ? 'check' : 'generate', inputs: inputs.length, cases: cases.length, provenance }) + '\n');
