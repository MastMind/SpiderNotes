import {
  RangeSetBuilder,
  StateEffect,
  StateField,
  Text,
  type Extension,
} from "@codemirror/state";
import {
  Decoration,
  DecorationSet,
  EditorView,
  WidgetType,
} from "@codemirror/view";
import { convertFileSrc } from "@tauri-apps/api/core";

// The vault's absolute root: local image/media sources in the markdown are
// resolved against it (set by App on vault open).
let vaultRoot = "";
export function setVaultRoot(p: string) {
  vaultRoot = p;
}

// The vault's local media server (127.0.0.1, set by App on vault open).
// WebKit's media pipeline plays large <video>/<audio> from plain HTTP, but
// not from the asset: scheme (its Range probe fails) nor from a large
// blob: URL (stalls in NETWORK_LOADING) — see media_server.rs.
let mediaPort = 0;
export function setMediaPort(p: number) {
  mediaPort = p;
}

// A local markdown src made absolute against the vault root (http(s)/
// data:/blob: pass through unchanged). Local srcs are percent-decoded —
// media imports encode spaces as %20 (a raw space would break the
// markdown src token).
function absPath(src: string): string {
  const s = src.trim();
  if (/^(https?:|data:|blob:)/i.test(s)) return s;
  let local = s;
  try {
    local = decodeURIComponent(s);
  } catch {
    // a malformed % sequence — use it raw
  }
  if (vaultRoot && !local.startsWith("/") && !/^[a-zA-Z]:/.test(local)) {
    return (
      vaultRoot.replace(/\\/g, "/").replace(/\/+$/, "") +
      "/" +
      local.replace(/^\.\/+/, "")
    );
  }
  return local;
}

// Resolve a markdown src for <img>/<video>/<audio>/<embed>: http(s)/data/
// blob pass through; a local path is made absolute against the vault root
// and served by the Tauri asset protocol (browser: left relative, it just
// won't load).
function resolveSrc(raw: string): string {
  const src = raw.trim();
  if (/^(https?:|data:|blob:)/i.test(src)) return src;
  const abs = absPath(src);
  if ((window as any).__TAURI_INTERNALS__) {
    try {
      return convertFileSrc(abs);
    } catch {
      // fall through to the raw path
    }
  }
  return src;
}

// A vault-relative markdown src as a loopback media-server URL. The server
// roots at the vault's hidden .assets dir, so `!.assets/foo.mp4` maps to
// /foo.mp4. null when the server is not running (plain browser) or the src
// is not a vault-relative media path.
function mediaHttpUrl(raw: string): string | null {
  if (!mediaPort) return null;
  const s = raw.trim();
  if (/^(https?:|data:|blob:)/i.test(s)) return null;
  let local = s;
  try {
    local = decodeURIComponent(s);
  } catch {
    // a malformed % sequence — use it raw
  }
  const rel = local.replace(/^\.\/+/, "");
  const path = rel.startsWith(".assets/") ? rel.slice(".assets/".length) : rel;
  if (!path) return null;
  return (
    `http://127.0.0.1:${mediaPort}/` +
    path.split("/").map(encodeURIComponent).join("/")
  );
}

// WebKit (macOS / Linux desktop) renders PDFs natively inside <embed>;
// Windows WebView2 and Android/iOS WebViews would show a blank page, so
// there a card opens the file in the system viewer instead.
function canInlinePdf(): boolean {
  const ua = navigator.userAgent;
  return /AppleWebKit|WebKit/i.test(ua) && !/(Android|iPhone|iPad|Windows)/i.test(ua);
}

// Open a LOCAL file in the system default app (opener plugin in the app,
// a new tab in a browser).
function openLocalFile(abs: string) {
  const w = window as any;
  if (/^https?:/i.test(abs)) {
    openExternal(abs);
    return;
  }
  if (w.__TAURI_INTERNALS__) {
    w.__TAURI_INTERNALS__
      .invoke("plugin:opener|open_path", { path: abs })
      .catch(() => {});
    return;
  }
  window.open(abs, "_blank");
}

// --- clickable widgets for the preview ---

// A task-list checkbox: replaces the literal `[ ]` / `[x]`; a click flips
// the three characters in the doc (the position is re-derived from the DOM
// on click, so mapped decorations stay correct).
class BoxWidget extends WidgetType {
  constructor(readonly checked: boolean) {
    super();
  }
  eq(o: BoxWidget) {
    return o.checked === this.checked;
  }
  toDOM(view: EditorView): HTMLElement {
    const el = document.createElement("span");
    el.className = "md-checkbox" + (this.checked ? " checked" : "");
    el.contentEditable = "false";
    el.addEventListener("click", () => {
      // the position is derived from the DOM at click time, so mapped
      // (stale-range) decorations stay correct
      const pos = view.posAtDOM(el);
      view.dispatch({
        changes: {
          from: pos,
          to: pos + 3,
          insert: this.checked ? "[ ]" : "[x]",
        },
        userEvent: "input.update",
      });
    });
    return el;
  }
}

// An inline image: replaces the whole `![alt](src)`; a load failure swaps
// the <img> for a dashed placeholder with the alt text.
class ImgWidget extends WidgetType {
  constructor(readonly src: string, readonly alt: string) {
    super();
  }
  eq(o: ImgWidget) {
    return o.src === this.src && o.alt === this.alt;
  }
  toDOM(): HTMLElement {
    const wrap = document.createElement("span");
    wrap.className = "md-img-wrap";
    wrap.contentEditable = "false";
    const img = document.createElement("img");
    img.className = "md-img";
    img.alt = this.alt;
    img.src = this.src;
    img.addEventListener("error", () => {
      img.remove();
      wrap.classList.add("md-img-broken");
      wrap.textContent = this.alt || "image";
    });
    wrap.appendChild(img);
    return wrap;
  }
}

// The WebKit media pipeline (AVFoundation on macOS/iOS, GStreamer on
// Linux) loads <video>/<audio> OUTSIDE the web process and never consults
// the custom asset: URL handler — the element errors while the very same
// URL works in <img> and fetch. So for asset: sources we fetch in the web
// process and hand the element a blob: URL. A Map deduplicates per source
// (one blob per file, shared by every widget that references it) and a
// refcount revokes the blob when the last widget using it is gone.
const blobUrls = new Map<string, string>();
const blobPromises = new Map<string, Promise<string>>();
const blobRefs = new Map<string, number>();

function isAssetSrc(src: string): boolean {
  return /^asset:\/\//i.test(src) || /^https?:\/\/asset\.localhost\//i.test(src);
}

function assetBlobUrl(src: string): Promise<string> {
  let p = blobPromises.get(src);
  if (!p) {
    p = (async () => {
      const res = await fetch(src);
      if (!res.ok) throw new Error(`asset ${res.status}`);
      const buf = await res.arrayBuffer();
      const type = res.headers.get("content-type") || undefined;
      const url = URL.createObjectURL(new Blob([buf], { type }));
      blobUrls.set(src, url);
      return url;
    })();
    p.catch(() => {
      blobPromises.delete(src); // allow a retry from a later widget
    });
    blobPromises.set(src, p);
  }
  return p;
}

function releaseAssetBlob(src: string) {
  const n = (blobRefs.get(src) ?? 0) - 1;
  if (n > 0) {
    blobRefs.set(src, n);
    return;
  }
  blobRefs.delete(src);
  // revoke only once the blob exists (a still-pending fetch keeps its
  // entry around and is released by a later decrement)
  const url = blobUrls.get(src);
  if (url) {
    URL.revokeObjectURL(url);
    blobUrls.delete(src);
    blobPromises.delete(src);
  }
}

// An inline audio/video element for media files referenced as images
// (`![demo](demo.mp4)`). `src` is the loopback media-server URL when one is
// running (WebKit plays large files from HTTP); `fallback` is the asset:
// URL used when the server 404s (media that is not under .assets/).
class MediaWidget extends WidgetType {
  private refHeld = false;
  constructor(
    readonly kind: "video" | "audio",
    readonly src: string,
    readonly fallback: string | null = null,
  ) {
    super();
  }
  eq(o: MediaWidget) {
    return o.kind === this.kind && o.src === this.src;
  }
  // The player controls are interactive: without this CodeMirror treats a
  // click/drag on the seek bar or volume button as editor input (stealing
  // the selection and killing playback), so the controls appear dead.
  ignoreEvent(): boolean {
    return true;
  }
  // the view calls this when the widget's DOM is removed
  destroy(_dom: HTMLElement): void {
    void _dom;
    if (this.refHeld) {
      this.refHeld = false;
      releaseAssetBlob(this.src);
    }
  }
  private armAsset(el: HTMLMediaElement, src: string): void {
    this.refHeld = true;
    blobRefs.set(src, (blobRefs.get(src) ?? 0) + 1);
    assetBlobUrl(src)
      .then((blob) => {
        // the widget was already removed: the blob is released elsewhere
        if ((blobRefs.get(src) ?? 0) < 1) return;
        el.src = blob;
      })
      .catch(() => {
        // fetch failed (offline etc.) — the direct src stays
      });
  }
  toDOM(): HTMLElement {
    const wrap = document.createElement("span");
    wrap.className = "md-media-wrap";
    wrap.contentEditable = "false";
    const el = document.createElement(this.kind);
    el.className = this.kind === "video" ? "md-video" : "md-audio";
    el.controls = true;
    if (this.kind === "video") {
      el.preload = "metadata";
      (el as HTMLVideoElement).playsInline = true;
    }
    el.src = this.src;
    if (isAssetSrc(this.src)) {
      this.armAsset(el, this.src);
    } else if (this.fallback) {
      // the loopback 404s when the file is not under .assets/ (media
      // imported before the hidden dir) — fall back to the asset: URL
      const fb = this.fallback;
      el.addEventListener(
        "error",
        () => {
          el.src = fb;
          this.armAsset(el, fb);
        },
        { once: true },
      );
    }
    wrap.appendChild(el);
    return wrap;
  }
}

// A PDF referenced as `![doc](doc.pdf)`: on WebKit (macOS / Linux desktop)
// the first page (and the whole document) renders inline via <embed>;
// elsewhere a card opens the file in the system viewer on click.
class PdfWidget extends WidgetType {
  constructor(readonly src: string, readonly name: string, readonly abs: string) {
    super();
  }
  eq(o: PdfWidget) {
    return o.src === this.src;
  }
  toDOM(): HTMLElement {
    const wrap = document.createElement("span");
    wrap.className = "md-pdf-wrap";
    wrap.contentEditable = "false";
    if (canInlinePdf()) {
      const el = document.createElement("embed");
      el.className = "md-pdf";
      el.type = "application/pdf";
      el.src = this.src;
      wrap.appendChild(el);
    } else {
      const card = document.createElement("span");
      card.className = "md-pdf-card";
      card.title = this.abs;
      card.textContent = "PDF · " + this.name;
      card.addEventListener("click", () => openLocalFile(this.abs));
      wrap.appendChild(card);
    }
    return wrap;
  }
}

// A clickable card for an http(s) link: the whole [text](url) syntax is
// replaced by a preview chip (globe + host); a click opens the URL.
class LinkCardWidget extends WidgetType {
  constructor(readonly url: string) {
    super();
  }
  eq(o: LinkCardWidget) {
    return o.url === this.url;
  }
  toDOM(): HTMLElement {
    let host = this.url;
    try {
      host = new URL(this.url).host;
    } catch {
      // keep the raw url
    }
    const wrap = document.createElement("span");
    wrap.className = "md-link-card";
    wrap.contentEditable = "false";
    wrap.setAttribute("data-url", this.url);
    wrap.title = this.url;
    const ico = document.createElement("span");
    ico.className = "md-link-ico";
    ico.innerHTML =
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="9.5"/><path d="M2.5 12h19"/><path d="M12 2.5a14.5 14.5 0 0 1 3.8 9.5 14.5 14.5 0 0 1-3.8 9.5 14.5 14.5 0 0 1-3.8-9.5 14.5 14.5 0 0 1 3.8-9.5z"/></svg>';
    const txt = document.createElement("span");
    txt.className = "md-link-text";
    txt.textContent = host;
    wrap.appendChild(ico);
    wrap.appendChild(txt);
    wrap.addEventListener("click", () => openExternal(this.url));
    return wrap;
  }
}

// --- block-level (line) patterns ---
const RE_HEADING = /^(#{1,6})(\s)/;
const RE_HR = /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/;
const RE_QUOTE = /^(\s*>)/;
// GFM task-list item: a bullet followed by `[ ]` or `[x]`
const RE_TASK = /^(\s*)([-*+])(\s)(\[( |x|X)\])(\s)/;
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
// `![alt](src)` — scanned before RE_LINK so the overlap guard keeps the
// link scanner away from it; media extensions become audio/video elements,
// pdf a preview (inline on WebKit, a card elsewhere)
const RE_IMAGE = /!\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+["'][^)]*)?\)/g;
const RE_AUDIO_EXT = /\.(mp3|wav|m4a|flac|aac|opus|ogg)$/i;
const RE_VIDEO_EXT = /\.(mp4|webm|ogv|mov)$/i;
const RE_PDF_EXT = /\.pdf$/i;
const RE_LINK = /\[([^\]]+)\]\(([^)]+)\)/g;
const RE_TAG = /(^|\s)(#[\p{L}\p{N}_-]+)/gu;

// Special collectInline class prefixes carrying widget payloads
const P_IMG = "@img:";
const P_MEDIA = "@media:";
const P_LINK = "@link:";
const P_CARD = "@card:";

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
  // ![alt](src) — replace the whole syntax with an image (or, for media
  // extensions, an audio/video element; for pdf, a preview)
  scan(RE_IMAGE, (m) => {
    const src = m[2].trim();
    const name = src.split("/").pop() || src;
    const payload = RE_AUDIO_EXT.test(name)
      ? P_MEDIA + "audio|" + encodeURIComponent(src)
      : RE_VIDEO_EXT.test(name)
        ? P_MEDIA + "video|" + encodeURIComponent(src)
        : RE_PDF_EXT.test(name)
          ? P_MEDIA + "pdf|" + encodeURIComponent(src)
          : P_IMG + encodeURIComponent(src) + "|" + encodeURIComponent(m[1]);
    return [[m.index, m.index + m[0].length, payload]];
  });
  // [text](url) — http(s) links become a preview card (whole syntax
  // replaced); other links (mailto: …) keep the styled-text treatment
  scan(RE_LINK, (m) => {
    const url = m[2].trim();
    if (/^https?:\/\//i.test(url)) {
      return [[m.index, m.index + m[0].length, P_CARD + encodeURIComponent(url)]];
    }
    const textStart = m.index + 1;
    const textEnd = textStart + m[1].length;
    return [
      [m.index, textStart, "md-hide"],
      [textStart, textEnd, P_LINK + encodeURIComponent(url)],
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
  // startSide order: block replace (-3e8-1) < line (-2e8) < mark (5e8).
  type Item = { from: number; to: number; deco: Decoration; side: number };
  const items: Item[] = [];
  const hidden = idHideRanges(doc);
  const hiddenLines = new Set<number>();
  for (const [f, t] of hidden) {
    for (let i = 1; i <= doc.lines; i++) {
      const ln = doc.line(i);
      if (ln.from >= f && ln.from < t) hiddenLines.add(i);
    }
    // Both flags matter, for different CodeMirror internals:
    // - `block: true`: a plain (inline) replace spanning line breaks
    //   materializes as an inline widget inside the NEXT line, forcing an
    //   empty line start before the following line decoration is read —
    //   the heading/list classes of the first visible line are dropped.
    // - `inclusiveEnd: false`: the default block replace ends with
    //   endSide +2e8+1, which sorts a line decoration at the same position
    //   AFTER the replace (into a later RangeSet layer). The tile builder
    //   then starts the first visible line before it reads the line
    //   decoration, and the pending line attributes are silently dropped.
    //   With a non-inclusive end (endSide -6e8+1) the line decoration sorts
    //   right after the replace, in the same layer, before the first mark
    //   span of the visible line.
    // (startSide -3e8-1 < line -2e8 < mark 5e8, hence side -1 in the sort.)
    items.push({ from: f, to: t, deco: Decoration.replace({ block: true, inclusiveEnd: false }), side: -1 });
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
      } else if ((m = RE_TASK.exec(text))) {
        addLine(Decoration.line({ class: "md-li md-task" }));
        // hide the bullet character (the checkbox is the visible marker)
        addMark(
          from + m[1].length,
          from + m[1].length + m[2].length,
          "md-hide",
        );
        // replace `[ ]` / `[x]` with a clickable checkbox widget
        const boxStart = from + m[1].length + m[2].length + m[3].length;
        // CM 6.43+: replace takes a spec object; a bare WidgetType is
        // silently dropped (rendered as an empty span)
        items.push({
          from: boxStart,
          to: boxStart + 3,
          deco: Decoration.replace({ widget: new BoxWidget(m[4] !== "[ ]") }),
          side: 1,
        });
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
        if (cls.startsWith(P_IMG)) {
          const [src, alt] = cls.slice(P_IMG.length).split("|");
          items.push({
            from: s,
            to: e,
            deco: Decoration.replace({
              widget: new ImgWidget(
                resolveSrc(decodeURIComponent(src)),
                decodeURIComponent(alt),
              ),
            }),
            side: 1,
          });
        } else if (cls.startsWith(P_MEDIA)) {
          const [kind, src] = cls.slice(P_MEDIA.length).split("|");
          const raw = decodeURIComponent(src);
          let name = raw.split("/").pop() || raw;
          try {
            name = decodeURIComponent(name);
          } catch {
            // keep as-is
          }
          const assetSrc = resolveSrc(raw);
          const httpSrc = mediaHttpUrl(raw);
          items.push({
            from: s,
            to: e,
            deco: Decoration.replace({
              widget:
                kind === "pdf"
                  ? new PdfWidget(assetSrc, name, absPath(raw))
                  : new MediaWidget(
                      kind === "video" ? "video" : "audio",
                      httpSrc ?? assetSrc,
                      httpSrc ? assetSrc : null,
                    ),
            }),
            side: 1,
          });
        } else if (cls.startsWith(P_CARD)) {
          items.push({
            from: s,
            to: e,
            deco: Decoration.replace({
              widget: new LinkCardWidget(decodeURIComponent(cls.slice(P_CARD.length))),
            }),
            side: 1,
          });
        } else if (cls.startsWith(P_LINK)) {
          const url = decodeURIComponent(cls.slice(P_LINK.length));
          items.push({
            from: s,
            to: e,
            deco: Decoration.mark({
              class: "md-link",
              attributes: { "data-url": url },
            }),
            side: 2,
          });
        } else {
          addMark(s, e, cls);
        }
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

// Open an external (http/https) link: the Tauri opener plugin in the
// app, a new browser tab otherwise.
function openExternal(url: string) {
  const w = window as any;
  if (w.__TAURI_INTERNALS__) {
    w.__TAURI_INTERNALS__
      .invoke("plugin:opener|open_url", { url })
      .catch(() => window.open(url, "_blank", "noopener"));
    return;
  }
  window.open(url, "_blank", "noopener");
}

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
    // clicking a rendered [text](url) opens the URL (the URL itself is
    // hidden — it rides along in a data attribute on the mark)
    EditorView.domEventHandlers({
      click(event, view) {
        void view;
        const t = event.target as HTMLElement | null;
        const link = t?.closest?.(".md-link") as HTMLElement | null;
        if (!link) return false;
        const url = link.getAttribute("data-url") || "";
        if (!/^https?:/i.test(url)) return false;
        event.preventDefault();
        openExternal(url);
        return true;
      },
    }),
  ];
}
