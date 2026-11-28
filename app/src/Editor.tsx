import { useEffect, useRef, type ChangeEvent } from "react";
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
import { importMediaFile } from "./api";
import { PaperclipIcon } from "./icons";

/** .cm-content with the live EditorView attached (browser check scripts). */
interface SnContentDOM extends HTMLDivElement {
  snView?: EditorView;
}

// Media the drop/attach handlers will import (MIME first, extension as
// fallback for files whose type the OS left empty).
const RE_MEDIA_EXT =
  /\.(png|jpe?g|gif|webp|bmp|svg|avif|ico|mp3|wav|m4a|flac|aac|opus|ogg|oga|mp4|webm|ogv|mov|pdf)$/i;
function isMediaFile(f: File) {
  return (
    f.type.startsWith("image/") ||
    f.type.startsWith("video/") ||
    f.type.startsWith("audio/") ||
    f.type === "application/pdf" ||
    RE_MEDIA_EXT.test(f.name)
  );
}

// A bare URL (pasted or dropped) becomes a link card, not raw text.
const RE_URL = /^https?:\/\/\S+$/i;
function urlHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** ArrayBuffer -> base64 (chunked: fromCharCode spread would overflow the
 *  stack on multi-MB files). */
function bufToB64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** Insert `text` at `pos`, on its own line(s), separated from the
 *  surrounding text by blank lines (no doubling at line/doc boundaries). */
function insertBlock(view: EditorView, text: string, pos: number) {
  const doc = view.state.doc;
  const before = pos > 0 ? doc.sliceString(pos - 1, pos) : "\n";
  const after = pos < doc.length ? doc.sliceString(pos, pos + 1) : "\n";
  let t = text;
  if (before !== "\n") t = "\n\n" + t;
  if (after !== "\n") t += "\n";
  view.dispatch({
    changes: { from: pos, insert: t },
    selection: { anchor: pos + t.length },
    userEvent: "input.drop",
  });
}

/** A pasted/dropped bare URL becomes a `[host](url)` link (rendered as a
 *  clickable preview card). */
function insertUrl(view: EditorView, url: string, pos: number) {
  insertBlock(view, `[${urlHost(url)}](${url})`, pos);
}

/** Copy the dropped/picked files into the vault (hidden .assets/ dir) and
 *  insert one `![name](rel)` per file at `pos` — the live preview renders
 *  images and gifs as <img>, audio/video extensions as players, pdf as a
 *  preview. Runs async; `onError` reports the files that could not be
 *  imported. */
async function importMedia(
  view: EditorView,
  files: File[],
  pos: number,
  onError: (msg: string) => void,
) {
  const snippets: string[] = [];
  for (const f of files) {
    try {
      const b64 = bufToB64(await f.arrayBuffer());
      const rel = await importMediaFile(f.name, b64);
      // spaces are %20 in the markdown src (the renderer decodes them back)
      snippets.push(
        `![${f.name.replace(/\.[^.]+$/, "")}](${rel.replace(/ /g, "%20")})`,
      );
    } catch (e) {
      onError(`cannot import ${f.name}: ${String(e)}`);
    }
  }
  if (!snippets.length) return;
  insertBlock(view, snippets.join("\n\n"), pos);
}

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
  onMediaError,
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
  /** Reports a dropped file that could not be imported into the vault. */
  onMediaError?: (msg: string) => void;
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
  const onMediaErrorRef = useRef(onMediaError);
  onMediaErrorRef.current = onMediaError;
  // The attach button's file input: the native picker works on every
  // platform, where OS drag&drop does not reach the webview (Android in
  // particular has no external file drops).
  const fileRef = useRef<HTMLInputElement>(null);
  const onFilesPicked = (e: ChangeEvent<HTMLInputElement>) => {
    const input = e.target;
    const picked = Array.from(input.files ?? []);
    const files = picked.filter(isMediaFile);
    input.value = "";
    const v = viewRef.current;
    if (!v) return;
    if (files.length < picked.length) {
      onMediaErrorRef.current?.(
        "only images, gifs, audio, video and pdf files are supported",
      );
    }
    if (files.length) {
      void importMedia(
        v,
        files,
        v.state.selection.main.head,
        (m) => onMediaErrorRef.current?.(m),
      );
    }
  };

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
        // Drag & drop media: the file is copied into the vault's hidden
        // .assets/ dir and a ![name](rel) is inserted at the drop point. These
        // handlers run BEFORE CM's built-in drop handler (which would read
        // the file as text and paste its bytes), so a file drop must
        // return true.
        EditorView.domEventHandlers({
          dragover(event) {
            const dt = (event as DragEvent).dataTransfer;
            if (dt?.types.includes("Files")) {
              dt.dropEffect = "copy";
              return true; // preventDefault -> the browser allows the drop
            }
            return false;
          },
          drop(event, view) {
            const dt = (event as DragEvent).dataTransfer;
            const files = dt ? Array.from(dt.files) : [];
            const media = files.filter(isMediaFile);
            if (media.length) {
              const pos =
                view.posAtCoords(
                  { x: event.clientX, y: event.clientY },
                  false,
                ) ?? view.state.selection.main.head;
              void importMedia(view, media, pos, (m) =>
                onMediaErrorRef.current?.(m),
              );
              return true;
            }
            // a URL dragged from a browser becomes a link card, not raw
            // text; any other text drop keeps CM's default handling
            const txt = (
              (dt?.getData("text/uri-list") || dt?.getData("text/plain") || "")
                .split("\n")[0] ?? ""
            ).trim();
            if (RE_URL.test(txt)) {
              const pos =
                view.posAtCoords(
                  { x: event.clientX, y: event.clientY },
                  false,
                ) ?? view.state.selection.main.head;
              insertUrl(view, txt, pos);
              return true;
            }
            return false;
          },
          paste(event, view) {
            const text =
              (event as ClipboardEvent).clipboardData?.getData("text/plain")
                ?.trim() ?? "";
            if (RE_URL.test(text)) {
              insertUrl(view, text, view.state.selection.main.head);
              return true;
            }
            return false; // CM's default paste
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
    // the browser check scripts reach the view through the DOM
    (view.contentDOM as SnContentDOM).snView = view;
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
    <div className="editor-wrap">
      <button
        className="attach-btn"
        title="Insert media (image, gif, audio, video, pdf)"
        onClick={() => fileRef.current?.click()}
      >
        <PaperclipIcon />
      </button>
      <input
        ref={fileRef}
        type="file"
        multiple
        hidden
        accept="image/*,video/*,audio/*,application/pdf,.pdf,.gif,.mp4,.webm,.mp3,.m4a,.ogg,.wav"
        onChange={onFilesPicked}
      />
      <div ref={ref} className={`editor${live ? "" : " md-raw"}`} />
    </div>
  );
}
