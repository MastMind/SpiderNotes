// Regression check for livePreview.ts decoration building (EditorState level).
//
// History, bug 1: computePreviewDecos used to add the hidden frontmatter
// ranges to the RangeSetBuilder *before* the per-line decorations. When the
// hidden range does not start at 0 (a user's own frontmatter lines keep the
// opening fence visible), the builder throws "Ranges must be added sorted by
// `from` position and `startSide`". That throw escaped a React effect and
// unmounted the whole app -> opening such a note showed a blank screen.
//
// History, bug 2 (NOT catchable at this level): the preview DecorationSet was
// wired via EditorView.decorations.of(view => view.state.field(...)) — a
// dynamic (function) provider. CM6 forbids line-break-replacing decorations
// in dynamic providers and the VIEW throws "Decorations that replace line
// breaks may not be specified via plugins" -> blank screen on ANY note with
// frontmatter. Fix: EditorView.decorations.from(previewField) (static
// derivation, like folding). See check-live-preview-browser.mjs, which runs
// the real bundle in headless Chromium and catches exactly that.
//
// Run: node app/scripts/check-live-preview.mjs   (exit 1 on any throw)

import ts from "typescript";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(here, "..");
const src = readFileSync(path.join(appDir, "src", "livePreview.ts"), "utf8");
const js = ts.transpileModule(src, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText;
const outDir = path.join(appDir, ".repro");
mkdirSync(outDir, { recursive: true });
const bundlePath = path.join(outDir, "livePreview.mjs");
writeFileSync(bundlePath, js);

const { livePreviewExtensions } = await import(
  pathToFileURL(bundlePath).href
);
const { EditorState } = await import(path.join(appDir, "node_modules/@codemirror/state/dist/index.js"));

const exts = livePreviewExtensions();
const previewField = exts[1];
const makeState = (doc) => EditorState.create({ doc, extensions: exts });

const cases = {
  idOnlyUserNote: `---\nspidernotes-id: 20c0b27d-3885-49b4-acbf-513e1f195873\n---\n\n\n# Первая заметка\n\nТут что-то написано`,
  mixedFrontmatter: `---\ntitle: foo\nspidernotes-id: 11111111-2222-3333-4444-555555555555\ntags: a b\n---\n\nbody text\n`,
  empty: "",
  noFrontmatter: `hello world\n`,
  idOnlyNoBody: `---\nspidernotes-id: abc\n---\n`,
  idOnlyBodyNoBlank: `---\nspidernotes-id: abc\n---\nbody\n`,
  userEmptyBlock: `---\n---\n\nbody\n`,
  twoIdLines: `---\nspidernotes-id: a\nspidernotes-id: b\n---\n\nbody\n`,
  idOnlyNoTrailingNL: `---\nspidernotes-id: abc\n---`,
  headingsListsQuotes:
    "# h1\n## h2\n\n- item **bold** *it* `code`\n- [[note]] #tag\n\n> quote\n\n1. one\n2. two\n\n---\n\nhr after\n",
  codeFences: "before\n\n```rust\nlet x = 1;\n```\n\nafter\n",
};

let fail = 0;
for (const [name, doc] of Object.entries(cases)) {
  try {
    const st = makeState(doc);
    st.field(previewField);
    // the [doc] effect replaces the whole document — exercise update()
    const st2 = st.update({
      changes: { from: 0, to: st.doc.length, insert: doc + " x" },
    }).state;
    st2.field(previewField);
    console.log(`ok   ${name}`);
  } catch (e) {
    fail++;
    console.log(`FAIL ${name}: ${e.constructor.name}: ${e.message}`);
  }
}
console.log(fail ? `${fail} case(s) threw` : "all cases pass");
process.exit(fail ? 1 : 0);
