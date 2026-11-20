import {
  RangeSetBuilder,
  StateEffect,
  StateField,
  Text,
  type Extension,
} from "@codemirror/state";
import { Decoration, DecorationSet, EditorView } from "@codemirror/view";

// --- block-level (line) patterns ---
const RE_HEADING = /^(#{1,6})(\s)/;
const RE_HR = /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/;
const RE_QUOTE = /^(\s*>)/;
const RE_UL = /^(\s*)([-*+])(\s)/;
const RE_OL = /^(\s*)(\d+)([.)])(\s)/;
const RE_FENCE = /^\s*(```|~~~)/;

// --- inline patterns, scanned in priority order ---
const RE_CODE = /(`+)([^`]+)\1/g;
const RE_BOLDITALIC = /\*\*\*([^*]+)\*\*\*/g;
const RE_BOLD = /\*\*([^*]+)\*\*|__([^_]+)__/g;
const RE_STRIKE = /~~([^~]+)~~/g;
const RE_ITALIC =
  /\*([^*\s][^*]*)\*|(?<![\p{L}\p{N}])_([^_\s][^_]*)_(?![\p{L}\p{N}])/gu;
const RE_WIKILINK = /\[\[([^\]]+)\]\]/g;
const RE_LINK = /\[([^\]]+)\]\(([^)]+)\)/g;
const RE_TAG = /(^|\s)(#[\p{L}\p{N}_-]+)/gu;

type Range = [number, number, string];

/**
 * Inline ranges for a line. Emits both the marker ranges (rendered with
 * `md-hide`, i.e. invisible — WYSIWYG) and the content ranges (styled).
 */
function collectInline(line: string, lineFrom: number): Range[] {
  const out: Range[] = [];
  const covered: Array<[number, number]> = [];
  const overlaps = (a: number, b: number) =>
    covered.some(([s, e]) => a < e && b > s);
  const add = (s: number, e: number, cls: string) => {
    if (e <= s || overlaps(s, e)) return;
    out.push([s + lineFrom, e + lineFrom, cls]);
    covered.push([s, e]);
  };
  const scan = (
    re: RegExp,
    handle: (m: RegExpExecArray) => Range[],
  ) => {
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(line))) {
      for (const r of handle(m)) add(r[0], r[1], r[2]);
    }
  };
  // `code` — hide backticks, style the text
  scan(RE_CODE, (m) => {
    const n = m[1].length;
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + n, "md-hide"],
      [m.index + n, end - n, "md-code"],
      [end - n, end, "md-hide"],
    ];
  });
  scan(RE_BOLDITALIC, (m) => {
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + 3, "md-hide"],
      [m.index + 3, end - 3, "md-bi"],
      [end - 3, end, "md-hide"],
    ];
  });
  scan(RE_BOLD, (m) => {
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + 2, "md-hide"],
      [m.index + 2, end - 2, "md-bold"],
      [end - 2, end, "md-hide"],
    ];
  });
  scan(RE_STRIKE, (m) => {
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + 2, "md-hide"],
      [m.index + 2, end - 2, "md-strike"],
      [end - 2, end, "md-hide"],
    ];
  });
  scan(RE_ITALIC, (m) => {
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + 1, "md-hide"],
      [m.index + 1, end - 1, "md-italic"],
      [end - 1, end, "md-hide"],
    ];
  });
  // [[wikilink]] — hide the brackets
  scan(RE_WIKILINK, (m) => {
    const end = m.index + m[0].length;
    return [
      [m.index, m.index + 2, "md-hide"],
      [m.index + 2, end - 2, "md-wikilink"],
      [end - 2, end, "md-hide"],
    ];
  });
  // [text](url) — hide brackets and the URL, style the text
  scan(RE_LINK, (m) => {
    const textStart = m.index + 1;
    const textEnd = textStart + m[1].length;
    return [
      [m.index, textStart, "md-hide"],
      [textStart, textEnd, "md-link"],
      [textEnd, m.index + m[0].length, "md-hide"],
    ];
  });
  // #tag — keep the hash, style as a pill
  scan(RE_TAG, (m) => [
    [m.index + m[1].length, m.index + m[0].length, "md-tag"],
  ]);
  return out;
}

/// Ranges to hide in live preview: the `spidernotes-id` frontmatter line.
/// If the block holds nothing but the id line, the whole block is hidden
/// (both fences, the id line, and the blank separator after it); the user's
/// own frontmatter lines stay visible. Raw mode shows everything as-is.
function idHideRanges(doc: Text): Array<[number, number]> {
  if (doc.lines < 2) return [];
  if (doc.line(1).text.trim() !== "---") return [];
  let closeLine = -1;
  for (let i = 2; i <= doc.lines; i++) {
    if (doc.line(i).text.trim() === "---") {
      closeLine = i;
      break;
    }
  }
  if (closeLine === -1) return [];
  const isId = (t: string) => /^\s*spidernotes-id\s*:/.test(t);
  const inner = Array.from(
    { length: closeLine - 2 },
    (_, k) => doc.line(k + 2).text.trim(),
  );
  if (!inner.some(isId)) return []; // a user's own empty `---` block: leave it
  const hasUserLine = inner.some((t) => t !== "" && !isId(t));
  if (hasUserLine) {
    // keep user frontmatter, hide only the id line(s)
    const out: Array<[number, number]> = [];
    for (let i = 2; i < closeLine; i++) {
      const ln = doc.line(i);
      if (isId(ln.text)) {
        out.push([ln.from, i < doc.lines ? doc.line(i + 1).from : ln.to]);
      }
    }
    return out;
  }
  // id-only block: hide from the opening fence through the closing fence's
  // newline, plus a trailing blank separator line if present
  let to = closeLine < doc.lines ? doc.line(closeLine + 1).from : doc.line(closeLine).to;
  if (closeLine < doc.lines && doc.line(closeLine + 1).text === "") {
    to =
      closeLine + 2 <= doc.lines
        ? doc.line(closeLine + 2).from
        : doc.line(closeLine + 1).to;
  }
  return [[0, to]];
}

export function computePreviewDecos(doc: Text): DecorationSet {
  // RangeSetBuilder.add throws unless ranges arrive sorted by `from`
  // (then by startSide). The hidden frontmatter ranges do NOT always start
  // at 0 (a user's own frontmatter lines keep the opening fence visible),
  // so collect everything and sort once before building.
  // startSide order: line (-2e8) < replace (5e8-1) < mark (5e8).
  type Item = { from: number; to: number; deco: Decoration; side: number };
  const items: Item[] = [];
  const hidden = idHideRanges(doc);
  const hiddenLines = new Set<number>();
  for (const [f, t] of hidden) {
    for (let i = 1; i <= doc.lines; i++) {
      const ln = doc.line(i);
      if (ln.from >= f && ln.from < t) hiddenLines.add(i);
    }
    items.push({ from: f, to: t, deco: Decoration.replace({}), side: 1 });
  }
  let inFence = false;
  for (let i = 1; i <= doc.lines; i++) {
    if (hiddenLines.has(i)) continue;
    const line = doc.line(i);
    const text = line.text;
    const from = line.from;

    const addLine = (deco: Decoration) =>
      items.push({ from, to: from, deco, side: 0 });
    const addMark = (fromR: number, toR: number, cls: string) =>
      items.push({ from: fromR, to: toR, deco: Decoration.mark({ class: cls }), side: 2 });

    if (RE_FENCE.test(text)) {
      addLine(Decoration.line({ class: "md-fence" }));
      inFence = !inFence;
    } else if (inFence) {
      addLine(Decoration.line({ class: "md-codeblock" }));
    } else {
      let m: RegExpExecArray | null;
      if ((m = RE_HEADING.exec(text))) {
        addLine(
          Decoration.line({ class: `md-heading md-h${m[1].length}` }),
        );
        // hide `## `
        addMark(from, from + m[0].length, "md-hide");
      } else if (RE_HR.test(text)) {
        addLine(Decoration.line({ class: "md-hr" }));
        addMark(from, line.to, "md-hide");
      } else if ((m = RE_QUOTE.exec(text))) {
        addLine(Decoration.line({ class: "md-quote" }));
        // hide `> `
        addMark(from, from + m[0].length, "md-hide");
      } else if ((m = RE_UL.exec(text))) {
        addLine(Decoration.line({ class: "md-li md-li-ul" }));
        // hide the bullet character
        addMark(
          from + m[1].length,
          from + m[1].length + m[2].length,
          "md-hide",
        );
      } else if ((m = RE_OL.exec(text))) {
        addLine(
          Decoration.line({
            class: "md-li md-li-ol",
            attributes: { "data-num": m[2] },
          }),
        );
        // hide `1.`
        addMark(
          from + m[1].length,
          from + m[1].length + m[2].length + m[3].length,
          "md-hide",
        );
      }

      for (const [s, e, cls] of collectInline(text, from)) {
        addMark(s, e, cls);
      }
    }
  }

  items.sort((a, b) => a.from - b.from || a.side - b.side);
  const builder = new RangeSetBuilder<Decoration>();
  for (const it of items) builder.add(it.from, it.to, it.deco);
  return builder.finish();
}

/// Toggle effect: `toggleLive.of(true|false)`.
export const toggleLive = StateEffect.define<boolean>();

/// Whether live preview (inline markdown rendering) is on.
export const liveField = StateField.define<boolean>({
  create: () => true,
  update: (val, tr) => {
    for (const e of tr.effects) if (e.is(toggleLive)) return e.value;
    return val;
  },
});

/// The actual decorations. Empty set when live preview is off (raw source).
/// `liveField` starts `true`, so the initial value is the live preview.
export const previewField = StateField.define<DecorationSet>({
  create: (state) => computePreviewDecos(state.doc),
  update: (set, tr) => {
    let live = tr.state.field(liveField);
    let toggled = false;
    for (const e of tr.effects) {
      if (e.is(toggleLive)) {
        live = e.value;
        toggled = true;
      }
    }
    if (!live) return Decoration.none;
    if (tr.docChanged) return computePreviewDecos(tr.newDoc);
    if (toggled) return computePreviewDecos(tr.state.doc);
    return set.map(tr.changes);
  },
});

export function livePreviewExtensions(): Extension[] {
  return [
    liveField,
    previewField,
    // Wire the field's DecorationSet into the view via `.from(field)` —
    // a static derivation, the same way @codemirror/language provides
    // folding decorations. Do NOT use `.of(view => view.state.field(...))`:
    // function-based (dynamic) decoration providers forbid decorations
    // that replace line breaks (the hidden frontmatter range), and the
    // view throws `RangeError: Decorations that replace line breaks may
    // not be specified via plugins` on construction -> the whole app
    // unmounts (blank screen) when any note with frontmatter is opened.
    EditorView.decorations.from(previewField),
  ];
}
