import { useEffect, useRef } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import {
  EditorView,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers as lineNumbersExt,
} from "@codemirror/view";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
} from "@codemirror/language";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import {
  autocompletion,
  completionKeymap,
  type CompletionContext,
} from "@codemirror/autocomplete";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";
import { oneDark } from "@codemirror/theme-one-dark";
import type { NoteSummary } from "./types";
import { themeById } from "./themes";
import { liveField, livePreviewExtensions, toggleLive } from "./livePreview";

function wikilinkSource(getNotes: () => NoteSummary[]) {
  return (ctx: CompletionContext) => {
    const before = ctx.matchBefore(/\[\[[\p{L}\p{N}/._-]*$/u);
    if (!before) return null;
    const notes = getNotes();
    if (!notes.length) return null;
    const query = before.text.slice(2).toLowerCase();
    const options = notes
      .filter(
        (n) =>
          n.path.toLowerCase().includes(query) ||
          (n.path.split("/").pop() ?? "").toLowerCase().includes(query),
      )
      .map((n) => {
        const base = (n.path.split("/").pop() ?? n.path).replace(/\.md$/, "");
        return {
          label: n.path,
          detail: base === n.path ? undefined : base,
          apply: (
            view: EditorView,
            completion: { label: string },
            from: number,
            to: number,
          ) => {
            view.dispatch({
              changes: { from, to, insert: completion.label + "]]" },
              selection: { anchor: from + completion.label.length },
            });
          },
        };
      });
    return { from: before.from, options };
  };
}

export function Editor({
  doc,
  onDoc,
  notes,
  live,
  theme,
  lineNumbers,
  onOpenNote,
  onOpenTag,
}: {
  doc: string;
  onDoc: (s: string) => void;
  notes: NoteSummary[];
  live: boolean;
  /** Theme id from THEMES (themes.ts) — the editor needs only its mode. */
  theme: string;
  /** Show the line-number gutter (design setting, default on). */
  lineNumbers: boolean;
  onOpenNote?: (target: string) => void;
  onOpenTag?: (tag: string) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const themeComp = useRef(new Compartment());
  const lineNumComp = useRef(new Compartment());
  const onDocRef = useRef(onDoc);
  onDocRef.current = onDoc;
  const notesRef = useRef(notes);
  notesRef.current = notes;
  const onOpenNoteRef = useRef(onOpenNote);
  onOpenNoteRef.current = onOpenNote;
  const onOpenTagRef = useRef(onOpenTag);
  onOpenTagRef.current = onOpenTag;

  useEffect(() => {
    if (!ref.current) return;
    const state = EditorState.create({
      doc,
      extensions: [
        // Line numbers behind a compartment: the gutter is a design
        // setting and can be toggled without recreating the view.
        lineNumComp.current.of(lineNumbers ? lineNumbersExt() : []),
        // Wrap long lines to the editor's width: the editor always stays
        // full-width (no horizontal scroll at any window size).
        EditorView.lineWrapping,
        highlightActiveLine(),
        highlightActiveLineGutter(),
        history(),
        foldGutter(),
        drawSelection(),
        dropCursor(),
        indentOnInput(),
        bracketMatching(),
        autocompletion({
          override: [wikilinkSource(() => notesRef.current)],
        }),
        livePreviewExtensions(),
        // In live mode, clicking a wikilink / tag opens it (raw mode = plain editing).
        EditorView.domEventHandlers({
          click(event: MouseEvent, view: EditorView) {
            if (!view.state.field(liveField)) return false;
            if (event.button !== 0 || event.shiftKey || event.metaKey || event.ctrlKey)
              return false;
            const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
            if (pos == null) return false;
            const line = view.state.doc.lineAt(pos);
            const text = line.text;
            const local = pos - line.from;
            let m: RegExpExecArray | null;
            const wl = /\[\[([^\]]+)\]\]/g;
            while ((m = wl.exec(text))) {
              if (local >= m.index && local <= m.index + m[0].length) {
                onOpenNoteRef.current?.(m[1]);
                return true;
              }
            }
            const tg = /(^|\s)(#[\p{L}\p{N}_-]+)/gu;
            while ((m = tg.exec(text))) {
              const s = m.index + m[1].length;
              if (local >= s && local <= m.index + m[0].length) {
                onOpenTagRef.current?.(m[1].slice(1));
                return true;
              }
            }
            return false;
          },
        }),
        highlightSelectionMatches(),
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          ...foldKeymap,
          ...completionKeymap,
          ...searchKeymap,
          indentWithTab,
        ]),
        markdown({ base: markdownLanguage, codeLanguages: languages }),
        themeComp.current.of(themeById(theme).dark ? oneDark : []),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onDocRef.current(u.state.doc.toString());
        }),
        EditorView.theme({
          "&": { height: "100%" },
          // font family + size of the note text come from the CSS vars
          // --note-font / --note-fs (the "Note font" design settings)
          ".cm-scroller": {
            overflow: "auto",
          },
        }),
      ],
    });
    const view = new EditorView({ state, parent: ref.current });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    if (v.state.doc.toString() !== doc) {
      v.dispatch({
        changes: { from: 0, to: v.state.doc.length, insert: doc },
        selection: { anchor: 0 },
      });
    }
  }, [doc]);

  // Sync the live/raw toggle (raw = plain markdown source, no rendering).
  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    if (v.state.field(liveField) !== live) {
      v.dispatch({ effects: toggleLive.of(live) });
    }
  }, [live]);

  // Switch the editor theme (oneDark for dark themes, default light otherwise).
  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    v.dispatch({
      effects: themeComp.current.reconfigure(themeById(theme).dark ? oneDark : []),
    });
  }, [theme]);

  // Line-number gutter on/off (design setting).
  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    v.dispatch({
      effects: lineNumComp.current.reconfigure(
        lineNumbers ? lineNumbersExt() : [],
      ),
    });
  }, [lineNumbers]);

  return (
    <div ref={ref} className={`editor${live ? "" : " md-raw"}`} />
  );
}
