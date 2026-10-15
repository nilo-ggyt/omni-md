// Freeze outputs from the preserved editor's real CodeMirror commands for portable Cargo tests.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

// The baseline is an explicit local input; ordinary verification never installs or executes Node.
const baseline = process.argv[2];
if (!baseline) {
  // Missing provenance must fail instead of resolving an unrelated globally installed command.
  throw new Error('usage: node scripts/generate-editing-reference.mjs BASELINE_DIRECTORY');
}
const requireBaseline = createRequire(path.resolve(baseline, 'package.json'));
const { EditorState, EditorSelection } = requireBaseline('@codemirror/state');
const { indentMore, indentLess } = requireBaseline('@codemirror/commands');
const { indentUnit } = requireBaseline('@codemirror/language');
const commandsPath = requireBaseline.resolve('@codemirror/commands');
const commandsSource = fs.readFileSync(commandsPath);

// Synthetic fixtures distinguish marker association, visual tab columns and JavaScript whitespace.
const inputs = [
  '', 'hello', 'first\nsecond\nthird', '\n\n', '  first\n    second\nlast\n',
  '\tfirst\n \tsecond\n\t third\n', '\t\tword', ' \t \tword',
  '\u00a0\u2003word\n\ufeff\tword', '\u0085word\n\u2028\u2029word',
  '  中文😀e\u0301\n\t𐐀 word\n', '    \n\t\n  ',
  '  a\r\n\tb\r\nc', '  a\r\tb\rc', '# title\n- item\n> quote\n',
];
const cases = [];
for (const input of inputs) {
  // A document has one configured separator; translate its logical offsets to native raw offsets.
  const separator = input.includes('\r\n') ? '\r\n' : input.includes('\r') ? '\r' : '\n';
  const base = EditorState.create({
    doc: input,
    extensions: [EditorState.lineSeparator.of(separator), indentUnit.of('  ')],
  });
  const logical = base.doc.toString();
  const scalarBoundaries = [0];
  for (const scalar of logical) {
    // Every candidate selection ends on a scalar boundary rather than inside an emoji surrogate pair.
    scalarBoundaries.push(scalarBoundaries.at(-1) + scalar.length);
  }
  const candidates = new Set([0, logical.length]);
  for (const index of [1, 2, 3, 4, Math.floor(scalarBoundaries.length / 2)]) {
    if (index < scalarBoundaries.length) {
      // Early prefix positions and the document midpoint exercise replacement-interior mapping.
      candidates.add(scalarBoundaries[index]);
    }
  }
  for (let index = 0; index < logical.length; index++) {
    if (logical[index] === '\n') {
      // Include both sides of every line break to catch selected-line endpoint exclusion.
      candidates.add(index);
      candidates.add(index + 1);
    }
  }
  for (const [action, command] of [['indent-more', indentMore], ['indent-less', indentLess]]) {
    for (const anchor of candidates) {
      for (const head of candidates) {
        // Each case invokes the upstream command with its own real state and transaction dispatcher.
        let state = base.update({ selection: EditorSelection.single(anchor, head) }).state;
        const before = state;
        const handled = command({ state, dispatch(transaction) { state = transaction.state; } });
        cases.push({ action, input, handled,
          anchor: before.sliceDoc(0, anchor).length,
          head: before.sliceDoc(0, head).length,
          text: state.sliceDoc(),
          anchor_after: state.sliceDoc(0, state.selection.main.anchor).length,
          head_after: state.sliceDoc(0, state.selection.main.head).length,
        });
      }
    }
  }
}
// Record exact executable-source provenance, independent of the new Rust implementation.
const output = {
  commands_sha256: crypto.createHash('sha256').update(commandsSource).digest('hex'),
  state_version: JSON.parse(fs.readFileSync(path.resolve(commandsPath, '../../../state/package.json'))).version,
  commands_version: JSON.parse(fs.readFileSync(path.resolve(commandsPath, '../../package.json'))).version,
  cases,
};
const destination = fileURLToPath(new URL('../crates/omni-markdown-core/src/markdown_document_buffer/editing_commands/reference_cases.json', import.meta.url));
fs.writeFileSync(destination, JSON.stringify(output) + '\n');
process.stdout.write(JSON.stringify({ cases: cases.length, commands_sha256: output.commands_sha256 }) + '\n');
