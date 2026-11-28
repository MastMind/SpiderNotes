// Browser-level regression check for livePreview.ts.
//
// WHY THIS EXISTS BESIDES check-live-preview.mjs:
// CodeMirror enforces "Decorations that replace line breaks may not be
// specified via plugins" only at the EDITOR VIEW level (DOM), not at
// EditorState level. The node check cannot catch that. This script builds
// the real bundle (vite), loads it in headless Chromium with a mocked
// Tauri IPC, opens every note shape and fails on any uncaught exception or
// an unmounted React root.
//
// Requires: chromium (or chrome) on PATH and a fresh `vite build`.
// Run: node app/scripts/check-live-preview-browser.mjs
//
// History: 9d864f1 provided the preview DecorationSet through
// EditorView.decorations.of(view => view.state.field(...)) — a dynamic
// (function) provider, in which line-break-replacing decorations are
// forbidden. The hidden frontmatter range triggered
// RangeError: Decorations that replace line breaks may not be specified
// via plugins -> uncaught in a React effect -> the whole app unmounted
// -> blank white screen when opening any note with frontmatter.
// Fix: EditorView.decorations.from(previewField) (static derivation,
// the same mechanism @codemirror/language uses for folding).

import { readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { extname } from "node:path";
import { fileURLToPath } from "node:url";
import path from "node:path";

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DIST = path.join(appDir, "dist");
try {
  readFileSync(path.join(DIST, "index.html"));
} catch {
  console.error("dist/ not found — run `vite build` first");
  process.exit(2);
}

// --- self-contained mock notes: every frontmatter shape that must work ---
const realNotes = {
  "id-only.md":
    "---\nspidernotes-id: 20c0b27d-3885-49b4-acbf-513e1f195873\n---\n\n\n# Первая заметка\n\nТут что-то написано\n\n- пункт один\n- пункт **два**\n",
  "new-note.md":
    "---\nspidernotes-id: 0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9\n---\n\n# новая (созданная)\n",
  "id-only-no-body.md": "---\nspidernotes-id: abc\n---\n",
  "mixed-frontmatter.md":
    "---\ntitle: тестовая\nspidernotes-id: deadbeef-0000-0000-0000-000000000000\n---\n\nтело заметки **жирный**\n",
  "plain.md": "просто текст без frontmatter\n",
  "empty.md": "",
  "rich.md":
    "# rich\n\n- [ ] open task\n- [x] done task\n\n![alt text](http://127.0.0.1:9/nopng.png)\n\n![ok](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==)\n\n![anim](anim.gif)\n\n![clip](clip.mp4)\n\n![tone](tone.mp3)\n\n![report](report.pdf)\n\n[example](https://example.com/page)\n",
};
// JS port of crates/core title_preview() (see check-tree-browser.mjs)
function titlePreview(content) {
  let body = content.split("\n");
  if (content.startsWith("---")) {
    const end = body.findIndex((l, i) => i > 0 && l.trim() === "---");
    if (end >= 0) body = body.slice(end + 1);
  }
  let ti = -1;
  for (let i = 0; i < body.length; i++) {
    if (/^\s*#{1,6}(?:$|[\s\t])/.test(body[i])) { ti = i; break; }
  }
  if (ti < 0) ti = body.findIndex((l) => l.trim() !== "");
  if (ti < 0) return { title: "", preview: "" };
  let title = body[ti].trimStart().replace(/^#+[\s\t]?/, "").replace(/#+\s*$/, "").trim();
  let preview = "";
  for (let i = ti + 1; i < body.length; i++) {
    if (body[i].trim()) {
      preview = body[i].trim().split(/\s+/).join(" ");
      const chars = [...preview];
      if (chars.length > 80) preview = chars.slice(0, 80).join("") + "…";
      break;
    }
  }
  return { title, preview };
}
const notes = Object.entries(realNotes).map(([p, content], i) => {
  const uuid = (
    content.match(/spidernotes-id:\s*(\S+)/)?.[1] ??
    `00000000-0000-0000-0000-${String(i).padStart(12, "0")}`
  );
  return {
    uuid,
    path: p,
    hlc: { ms: Date.now(), counter: 0, device: "local" },
    hash: "",
    deleted: false,
    content,
    ...titlePreview(content),
  };
});
// Encrypted notes: the on-disk "content" is fake ciphertext; the mock keeps
// the plaintext + password and emulates the Rust unlock/lock/read behavior.
const encryptedNotes = {
  "secret.md": {
    uuid: "eeeeeeee-0000-0000-0000-000000000001",
    path: "secret.md",
    title: "Secret",
    preview: "",
    password: "hunter2",
    plain: "# Secret\n\ntop secret body **hidden**\n",
    content: "---\nspidernotes-encrypted: true\n---\nAAAAfakeciphertext\n",
  },
};
notes.push(
  ...Object.values(encryptedNotes).map((n) => ({
    uuid: n.uuid,
    path: n.path,
    hlc: { ms: Date.now(), counter: 0, device: "local" },
    hash: "",
    deleted: false,
    encrypted: true,
    title: n.title,
    preview: n.preview,
    content: n.content,
  }))
);
const notesList = notes.map(({ content, ...s }) => s);

const mock = `
window.__cbs = {};
window.__invokeLog = [];
window.__mockNotes = ${JSON.stringify(Object.fromEntries(notes.map((n) => [n.uuid, n])))};
window.__mockNotesList = ${JSON.stringify(notesList)};
// encrypted-note emulation: uuid -> {password, plain, title}; __unlocked is
// the set of note uuids whose key the (mock) app state currently holds
window.__encNotes = ${JSON.stringify(
  Object.fromEntries(
    Object.values(encryptedNotes).map((n) => [
      n.uuid,
      { password: n.password, plain: n.plain, title: n.title },
    ])
  )
)};
window.__unlocked = new Set();
const hlc = { ms: 1, counter: 0, device: "local" };
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    window.__invokeLog.push(cmd);
    switch (cmd) {
      case "vault_status": return null;
      case "default_vault_path": return "/mock/vault";
      case "open_vault":
        return { path: "/mock/vault", p2p_port: 8788, device_id: "localdev", device_name: "mock", notes: window.__mockNotesList, peers: [], folders: [] };
      case "list_notes": return window.__mockNotesList;
      case "list_folders": return [];
      case "create_folder": return args.path;
      case "rename_folder": return 0;
      case "delete_folder": return [];
      case "read_note": {
        const n = window.__mockNotes[args.uuid];
        if (!n) throw new Error("unknown note " + args.uuid);
        // encrypted: plaintext only while unlocked (mirrors the Rust gate)
        if (n.encrypted) {
          if (!window.__unlocked.has(args.uuid))
            throw new Error("note is locked: enter its password first");
          return { uuid: n.uuid, path: n.path, content: window.__encNotes[n.uuid].plain, hlc, deleted: false };
        }
        return { uuid: n.uuid, path: n.path, content: n.content, hlc, deleted: false };
      }
      case "save_note": return { uuid: "x", path: args.path, hlc, hash: "", deleted: false };
      case "import_media_file": {
        window.__storedMedia = window.__storedMedia || {};
        window.__storedMedia[args.name] = { b64Len: String(args.b64).length };
        return ".assets/" + args.name;
      }
      case "asset_report": return {
        files: [
          { path: ".assets/photo.png", size: 1000 },
          { path: ".assets/old.gif", size: 500 },
          { path: ".assets/keep.mp4", size: 2000 },
        ],
        orphans: [".assets/old.gif"],
        encrypted_unchecked: 1,
        total_bytes: 3500,
        orphan_bytes: 500,
      };
      case "delete_assets": {
        window.__deletedAssets = args.paths;
        return 500;
      }
      case "delete_note": return { uuid: args.uuid, path: "?", hlc, hash: "", deleted: true };
      case "rename_note": return { uuid: args.uuid, path: args.newPath, hlc, hash: "", deleted: false };
      case "create_encrypted_note": {
        const name = args.path.replace(/\.md$/, "");
        const uuid = "eeeeeeee-0000-0000-0000-00000000" + String(Math.floor(Math.random() * 90) + 10);
        window.__encNotes[uuid] = { password: args.password, plain: "# " + name + "\\n", title: name };
        window.__unlocked.add(uuid);
        const s = { uuid, path: args.path, hlc, hash: "", deleted: false, encrypted: true, title: name, preview: "" };
        window.__mockNotes[uuid] = s;
        window.__mockNotesList.push(s);
        return s;
      }
      case "unlock_note": {
        const e = window.__encNotes[args.uuid];
        if (!e) throw new Error("note is not encrypted");
        if (e.password !== args.password) throw new Error("wrong password or corrupted note");
        window.__unlocked.add(args.uuid);
        return e.plain;
      }
      case "lock_note": { window.__unlocked.delete(args.uuid); return null; }
      case "search_notes": return [];
      case "backlinks": return [];
      case "all_tags": return [];
      case "notes_with_tag": return [];
      case "device_info": return { id: "localdev", name: "mock" };
      case "list_peers": return [];
      case "list_trusted": return [];
      case "trust_device": return [];
      case "untrust_device": return [];
      case "sync_with": return { peer_name: "?", sent: 0, applied: 0, skipped: 0 };
      case "sync_all": return [];
      case "p2p_settings": return { autosync_secs: 30, p2p_port: 8788, p2p_port_setting: 8788, p2p_running: true, last_sync: null };
      case "server_sync_status": return { enabled: false, connected: false, status: "server sync is off" };
      case "set_bg_sync": return null;
      case "set_autosync_secs": return args.secs;
      case "set_p2p_port_setting": return args.port;
      case "restart_p2p": return 8788;
      case "mdns_refresh": return null;
      case "plugin:event|listen": return 1;
      case "plugin:event|unlisten": return null;
      default: return null;
    }
  },
  transformCallback: (cb) => {
    const id = Math.floor(Math.random() * 1e9);
    window.__cbs[id] = cb;
    return id;
  },
  unregisterCallback: (id) => { delete window.__cbs[id]; },
  convertFileSrc: (p) => "asset://" + p,
};
// @tauri-apps/api/event's unlisten() calls into the event plugin's
// internals — a no-op in the mock
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: () => {},
};
// Emulate the Tauri asset protocol for fetch: MediaWidget fetches asset:
// URLs in the web process (the media pipeline does not consult the custom
// scheme handler) and swaps the element's src to a blob: URL. A real
// headless browser cannot load asset:, so we answer it here.
const __realFetch = window.fetch.bind(window);
window.__assetFetches = 0;
window.fetch = (input, init) => {
  const u = typeof input === "string" ? input : (input && input.url) || "";
  if (u.startsWith("asset://")) {
    window.__assetFetches++;
    const body = new Uint8Array([1, 2, 3, 4]);
    const ct = /mp4|webm|ogv|mov/i.test(u)
      ? "video/mp4"
      : /mp3|wav|m4a|flac|aac|opus|ogg/i.test(u)
        ? "audio/mpeg"
        : "application/octet-stream";
    return Promise.resolve(
      new Response(body, { status: 200, headers: { "content-type": ct } }),
    );
  }
  return __realFetch(input, init);
};
`;

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2" };
const server = createServer((req, res) => {
  let p = req.url.split("?")[0];
  if (p === "/") p = "/index.html";
  try {
    const body = readFileSync(path.join(DIST, p));
    res.writeHead(200, { "Content-Type": MIME[extname(p)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});
await new Promise((r) => server.listen(8124, r));

const chromeBin = process.env.CHROME_BIN || "chromium";
const chrome = spawn(chromeBin, [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  "--remote-debugging-port=9224", "--window-size=1280,800", "about:blank",
], { stdio: ["ignore", "pipe", "pipe"] });
chrome.stderr.on("data", () => {});

let targets = null;
for (let i = 0; i < 40; i++) {
  await new Promise((r) => setTimeout(r, 500));
  try {
    targets = await (await fetch("http://127.0.0.1:9224/json")).json();
    if (targets.some((t) => t.type === "page")) break;
  } catch {}
}
if (!targets) { console.error("chromium did not start"); process.exit(2); }

const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = () => j(new Error("ws error")); });

let idc = 0;
const pending = new Map();
const consoleLines = [];
const exceptions = [];
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  else if (msg.method === "Runtime.consoleAPICalled") {
    consoleLines.push(msg.params.args.map((a) => a.value ?? a.description ?? "").join(" "));
  } else if (msg.method === "Runtime.exceptionThrown") {
    const d = msg.params.exceptionDetails;
    exceptions.push(`${d.exception?.description || d.text}`);
  }
};
const send = (method, params = {}) =>
  new Promise((res) => {
    const id = ++idc;
    pending.set(id, res);
    ws.send(JSON.stringify({ id, method, params }));
  });

await send("Runtime.enable");
await send("Page.enable");
await send("Page.addScriptToEvaluateOnNewDocument", { source: mock });
await send("Page.navigate", { url: "http://127.0.0.1:8124/" });
await new Promise((r) => setTimeout(r, 3000));

const evalJs = (expression) =>
  send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }).then((m) => {
    if (m.result?.exceptionDetails)
      console.log("EVAL-EXC:", m.result.exceptionDetails.exception?.description || m.result.exceptionDetails.text);
    return m.result?.result?.value;
  });

const state = async (label) => {
  const s = await evalJs(`(() => {
    const root = document.getElementById("root");
    return JSON.stringify({
      rootChildren: root ? root.children.length : -1,
      cmEditor: !!document.querySelector(".cm-editor"),
      cmLines: document.querySelectorAll(".cm-line").length,
      cmText: Array.from(document.querySelectorAll(".cm-line")).map(l => l.textContent.slice(0, 50)),
      cmClasses: Array.from(document.querySelectorAll(".cm-line")).map(l => l.className),
      errorDiv: document.querySelector(".error")?.textContent || null,
    });
  })()`);
  console.log(`--- ${label}: ${s}`);
  return s;
};

let fail = 0;
const btn = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll("button")).find(b => /open.*vault/i.test(b.textContent));
  if (b) { b.click(); return true; } return false;
})()`);
if (!btn) { console.log("FAIL: vault button not found"); process.exit(1); }
await new Promise((r) => setTimeout(r, 2000));
await state("after open-vault");

for (const [p, content] of Object.entries(realNotes)) {
  const clicked = await evalJs(`(() => {
    const btns = Array.from(document.querySelectorAll("button.tree-item"));
    const el = btns.find(b => b.title === ${JSON.stringify(p)});
    if (el) { el.click(); return "clicked " + ${JSON.stringify(p)}; }
    return "note item not found: " + ${JSON.stringify(p)};
  })()`);
  console.log(clicked);
  await new Promise((r) => setTimeout(r, 1200));
  const s = JSON.parse((await state(`note: ${p}`)) ?? "{}");
  if (s.rootChildren === 0) { console.log("FAIL: React root unmounted (blank screen)"); fail++; break; }
  if (!s.cmEditor) { console.log("FAIL: editor did not render"); fail++; }
  // the spidernotes-id line must never be visible in live preview
  if (s.cmText.some((t) => t.includes("spidernotes-id"))) {
    console.log("FAIL: spidernotes-id line is visible in the editor");
    fail++;
  }
  // for notes with a body, the body must be visible
  const bodyLine = content.split("\n").find((l) => l.trim() && !l.trim().startsWith("---") && !l.includes("spidernotes-id"));
  if (bodyLine && !s.cmText.some((t) => t.includes(bodyLine.slice(0, 20)))) {
    console.log("FAIL: body content missing:", bodyLine.slice(0, 40));
    fail++;
  }
  // a heading line — especially the FIRST visible line right after a hidden
  // id-only frontmatter block — must carry its md-hN line class (regression:
  // the frontmatter replace decoration used to swallow the line decoration
  // of the line that follows it, rendering the title as plain text)
  let lines = content.split("\n");
  if (lines[0]?.trim() === "---") {
    const end = lines.findIndex((l, i) => i > 0 && l.trim() === "---");
    if (end >= 0) lines = lines.slice(end + 1);
  }
  const heading = lines.find((l) => /^#{1,6}\s/.test(l));
  if (heading) {
    const level = heading.match(/^#+/)[0].length;
    const idx = s.cmText.findIndex((t) => t.includes(heading.slice(0, 20)));
    const cls = idx >= 0 ? s.cmClasses[idx] : "";
    if (!cls.includes(`md-h${level}`)) {
      console.log(`FAIL: heading line lost its md-h${level} class (line: ${heading.slice(0, 40)}, class: ${cls || "not found"})`);
      fail++;
    }
  }
}

// ---- WP6: task checkboxes, inline images, media, links (rich.md) ----
await evalJs(`(() => {
  const el = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "rich.md");
  el && el.click();
  return !!el;
})()`);
await new Promise((r) => setTimeout(r, 1200));
const w6 = await evalJs(`(() => {
  const cbs = Array.from(document.querySelectorAll(".md-checkbox"));
  const imgBroken = document.querySelector(".md-img-wrap.md-img-broken");
  const imgOk = Array.from(document.querySelectorAll(".md-img")).some(i => i.naturalWidth > 0);
  const gifBroken = Array.from(document.querySelectorAll(".md-img-wrap.md-img-broken")).map(w => w.textContent).includes("anim");
  const vid = document.querySelector(".md-video");
  const aud = document.querySelector(".md-audio");
  const pdf = document.querySelector("embed.md-pdf");
  const link = document.querySelector(".md-link-card");
  return {
    cbs: cbs.map(c => c.className),
    imgBroken: imgBroken ? imgBroken.textContent : null,
    imgOk,
    gifBroken,
    hasVideo: !!vid,
    videoSrc: vid ? vid.getAttribute("src") : null,
    hasAudio: !!aud,
    audioSrc: aud ? aud.getAttribute("src") : null,
    hasPdf: !!pdf,
    pdfSrc: pdf ? pdf.getAttribute("src") : null,
    linkUrl: link ? link.getAttribute("data-url") : null,
    linkHost: link ? link.textContent : null,
  };
})()`);
console.log("rich.md widgets:", JSON.stringify(w6));
if (w6.cbs.length !== 2 || !w6.cbs[0].includes("md-checkbox") || w6.cbs[0].includes("checked") || !w6.cbs[1].includes("checked")) {
  console.log("FAIL: task checkboxes rendered (expected unchecked + checked)", JSON.stringify(w6.cbs));
  fail++;
}
if (w6.imgBroken !== "alt text") {
  console.log("FAIL: broken image placeholder shows the alt text", JSON.stringify(w6.imgBroken));
  fail++;
}
if (!w6.hasVideo) {
  console.log("FAIL: video file rendered as <video>", JSON.stringify({ v: w6.hasVideo }));
  fail++;
}
if (!w6.hasAudio) {
  console.log("FAIL: audio file rendered as <audio>", JSON.stringify({ a: w6.hasAudio }));
  fail++;
}
// The asset: src is fetched in the web process and swapped for a blob: URL
// (the WebKit media pipeline does not consult the custom scheme handler).
// Poll briefly: the fetch mock resolves on a microtask but the swap is async.
let mediaSrc = { v: w6.videoSrc || "", a: w6.audioSrc || "" };
for (let i = 0; i < 30; i++) {
  mediaSrc = await evalJs(`(() => ({
    v: document.querySelector(".md-video")?.getAttribute("src") || "",
    a: document.querySelector(".md-audio")?.getAttribute("src") || "",
  }))()`);
  if ((mediaSrc.v || "").startsWith("blob:") && (mediaSrc.a || "").startsWith("blob:")) break;
  await new Promise((r) => setTimeout(r, 100));
}
if (!(mediaSrc.v || "").startsWith("blob:")) {
  console.log("FAIL: video asset: src was not swapped for a blob: url", JSON.stringify(mediaSrc));
  fail++;
}
if (!(mediaSrc.a || "").startsWith("blob:")) {
  console.log("FAIL: audio asset: src was not swapped for a blob: url", JSON.stringify(mediaSrc));
  fail++;
}
if (!w6.imgOk) {
  console.log("FAIL: a loadable image (data: url) renders as a real <img>");
  fail++;
}
if (!w6.gifBroken) {
  console.log("FAIL: gif with an unloadable src shows the broken placeholder with its alt");
  fail++;
}
if (!w6.hasPdf || !(w6.pdfSrc || "").includes("report.pdf")) {
  console.log("FAIL: pdf reference renders a preview widget (embed on WebKit)", JSON.stringify({ p: w6.hasPdf, s: w6.pdfSrc }));
  fail++;
}
if (w6.linkUrl !== "https://example.com/page" || w6.linkHost !== "example.com") {
  console.log("FAIL: http link renders a preview card with url + host", JSON.stringify({ u: w6.linkUrl, h: w6.linkHost }));
  fail++;
}
// clicking the checkbox flips the [ ] in the doc
const cbClicked = await evalJs(`(() => {
  const cb = document.querySelector(".md-checkbox");
  cb && cb.click();
  return !!cb;
})()`);
await new Promise((r) => setTimeout(r, 600));
const w6b = await evalJs(`(() => {
  const v = document.querySelector(".cm-content")?.snView;
  const cbs = Array.from(document.querySelectorAll(".md-checkbox")).map(c => c.className);
  return { doc: v ? v.state.doc.toString() : null, cbs };
})()`);
if (!cbClicked || !w6b.doc || !w6b.doc.includes("- [x] open task") || !w6b.cbs[0].includes("checked")) {
  console.log("FAIL: checkbox click toggles [ ] -> [x] in the doc", JSON.stringify(w6b));
  fail++;
}
// clicking the link card asks the opener plugin to open the url
const linkClicked = await evalJs(`(() => {
  const l = document.querySelector(".md-link-card");
  l && l.click();
  return !!l;
})()`);
await new Promise((r) => setTimeout(r, 300));
const openerCalls = await evalJs(`(() => window.__invokeLog.filter(c => c === "plugin:opener|open_url").length)`);
if (!linkClicked || openerCalls < 1) {
  console.log("FAIL: link click invokes the opener plugin", `clicked=${linkClicked} calls=${openerCalls}`);
  fail++;
}

// ---- WP7: encrypted notes (lock icon, unlock dialog, plaintext) ----
// the encrypted note row carries a lock glyph
const lockIcon = await evalJs(`(() => {
  const row = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "secret.md");
  return !!(row && row.querySelector(".row-lock"));
})()`);
if (!lockIcon) { console.log("FAIL: encrypted note row shows a lock icon"); fail++; }
// clicking it opens the unlock (password) dialog, not the editor
await evalJs(`(() => {
  const row = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "secret.md");
  row && row.click();
})()`);
await new Promise((r) => setTimeout(r, 400));
const unlockDlg = await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent));
  const pw = dlg ? dlg.closest(".dlg").querySelector('input[type="password"]') : null;
  return { hasDlg: !!dlg, hasPw: !!pw };
})()`);
if (!unlockDlg.hasDlg || !unlockDlg.hasPw) {
  console.log("FAIL: opening an encrypted note shows the password dialog", JSON.stringify(unlockDlg));
  fail++;
}
// wrong password -> rejected (still locked, dialog stays)
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const pw = card && card.querySelector('input[type="password"]');
  const set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set;
  if (pw) { set.call(pw, "wrong"); pw.dispatchEvent(new Event("input", { bubbles: true })); }
  const btn = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /unlock/i.test(b.textContent));
  btn && btn.click();
})()`);
await new Promise((r) => setTimeout(r, 400));
const stillLocked = await evalJs(`(() => ({
  err: !!document.querySelector(".error"),
  dlgOpen: !!Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent)),
}))()`);
if (!stillLocked.dlgOpen) {
  console.log("FAIL: a wrong password keeps the note locked (dialog stays open)", JSON.stringify(stillLocked));
  fail++;
}
// correct password -> unlocks and the editor shows the plaintext
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const pw = card && card.querySelector('input[type="password"]');
  const set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set;
  if (pw) { set.call(pw, "hunter2"); pw.dispatchEvent(new Event("input", { bubbles: true })); }
  const btn = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /unlock/i.test(b.textContent));
  btn && btn.click();
})()`);
await new Promise((r) => setTimeout(r, 800));
const encOpened = await evalJs(`(() => ({
  dlgGone: !Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent)),
  cmText: Array.from(document.querySelectorAll(".cm-line")).map(l => l.textContent),
}))()`);
if (!encOpened.dlgGone || !encOpened.cmText.some(t => t.includes("top secret body"))) {
  console.log("FAIL: correct password opens the encrypted note's plaintext", JSON.stringify(encOpened));
  fail++;
}
// ---- WP7 (bug fix): deleting a LOCKED encrypted note must ask for the
// password FIRST, then show an in-app "delete?" confirm, and delete ONLY
// after that confirm. (window.confirm used to be unreliable in the webview,
// so the deletion could fire before the dialog the user was shown.) ----
// secret.md is selected + unlocked right now: switching to another note
// re-locks it (the frontend drops the held key)
await evalJs(`(() => {
  const el = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "plain.md");
  el && el.click();
  return !!el;
})()`);
await new Promise((r) => setTimeout(r, 500));
const relocked = await evalJs(`(() => !window.__unlocked.has("eeeeeeee-0000-0000-0000-000000000001"))()`);
if (!relocked) {
  console.log("FAIL: switching away re-locks the encrypted note");
  fail++;
}
// swipe secret.md to reveal the action layer (React renders it on the next
// tick after setSwipedUuid, so the Delete click goes in a separate step)
const encDelSwipe = await evalJs(`(() => {
  const b = document.querySelector('button.tree-item[title="secret.md"]');
  if (!b) return "no-note";
  const r = b.getBoundingClientRect();
  const x = r.x + r.width - 40, y = r.y + r.height / 2;
  const mk = (cx) => new Touch({ identifier: 90, target: b, clientX: cx, clientY: y, pageX: cx, pageY: y });
  b.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [mk(x)], targetTouches: [mk(x)], changedTouches: [mk(x)] }));
  b.dispatchEvent(new TouchEvent("touchmove", { bubbles: true, cancelable: true, touches: [mk(x - 55)], targetTouches: [mk(x - 55)], changedTouches: [mk(x - 55)] }));
  b.dispatchEvent(new TouchEvent("touchmove", { bubbles: true, cancelable: true, touches: [mk(x - 110)], targetTouches: [mk(x - 110)], changedTouches: [mk(x - 110)] }));
  b.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [mk(x - 110)] }));
  return "swiped";
})()`);
await new Promise((r) => setTimeout(r, 300));
const encDelBtn = await evalJs(`(() => {
  const del = Array.from(document.querySelectorAll(".row-act")).find(z => z.title === "Delete");
  del && del.click();
  return !!del;
})()`);
await new Promise((r) => setTimeout(r, 500));
// the PASSWORD dialog must appear first — no delete confirm, no delete yet
const encDelStep1 = await evalJs(`(() => ({
  unlockDlg: !!Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent)),
  deleteDlg: !!Array.from(document.querySelectorAll(".dlg-title")).find(t => /delete note/i.test(t.textContent)),
  deleted: window.__invokeLog.filter(c => c === "delete_note").length,
}))()`);
if (encDelSwipe !== "swiped" || !encDelBtn || !encDelStep1.unlockDlg || encDelStep1.deleteDlg || encDelStep1.deleted > 0) {
  console.log("FAIL: deleting a locked encrypted note must show the password dialog FIRST (no premature delete)", `swipe=${encDelSwipe} btn=${encDelBtn}`, JSON.stringify(encDelStep1));
  fail++;
}
// correct password -> the IN-APP delete confirm appears, still nothing deleted
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /unlock/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const pw = card && card.querySelector('input[type="password"]');
  const set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set;
  if (pw) { set.call(pw, "hunter2"); pw.dispatchEvent(new Event("input", { bubbles: true })); }
  const btn = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /unlock/i.test(b.textContent));
  btn && btn.click();
})()`);
await new Promise((r) => setTimeout(r, 500));
const encDelStep2 = await evalJs(`(() => ({
  deleteDlg: !!Array.from(document.querySelectorAll(".dlg-title")).find(t => /delete note/i.test(t.textContent)),
  deleted: window.__invokeLog.filter(c => c === "delete_note").length,
}))()`);
if (!encDelStep2.deleteDlg || encDelStep2.deleted > 0) {
  console.log("FAIL: after the password the in-app delete confirm must appear WITHOUT deleting", JSON.stringify(encDelStep2));
  fail++;
}
// cancel -> the note stays intact and is locked again
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /delete note/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const cancel = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /cancel/i.test(b.textContent));
  cancel && cancel.click();
  return !!cancel;
})()`);
await new Promise((r) => setTimeout(r, 500));
const encDelStep3 = await evalJs(`(() => ({
  dlgGone: !Array.from(document.querySelectorAll(".dlg-title")).find(t => /delete note/i.test(t.textContent)),
  deleted: window.__invokeLog.filter(c => c === "delete_note").length,
  stillThere: !!document.querySelector('button.tree-item[title="secret.md"]'),
  locked: !window.__unlocked.has("eeeeeeee-0000-0000-0000-000000000001"),
}))()`);
if (!encDelStep3.dlgGone || encDelStep3.deleted > 0 || !encDelStep3.stillThere || !encDelStep3.locked) {
  console.log("FAIL: canceling the delete confirm keeps the note intact and locked", JSON.stringify(encDelStep3));
  fail++;
}
// the "New note" button opens the create dialog with an encrypt checkbox
const newBtn = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".newnote-btn")).find(x => /new note/i.test(x.textContent));
  if (b) { b.click(); return true; } return false;
})()`);
await new Promise((r) => setTimeout(r, 300));
const createDlg = await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /new note/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  return {
    hasDlg: !!dlg,
    hasName: !!(card && card.querySelector('input.inp')),
    hasEncrypt: !!(card && card.querySelector('input[type="checkbox"]')),
  };
})()`);
if (!newBtn || !createDlg.hasDlg || !createDlg.hasName || !createDlg.hasEncrypt) {
  console.log("FAIL: 'New note' opens a dialog with name + encrypt checkbox", JSON.stringify({ newBtn, ...createDlg }));
  fail++;
}
// create an encrypted note (name + password twice) -> it appears, locked in the list
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /new note/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set;
  const name = card && card.querySelector('input.inp');
  if (name) { set.call(name, "created-secret"); name.dispatchEvent(new Event("input", { bubbles: true })); }
  const chk = card && card.querySelector('input[type="checkbox"]');
  chk && chk.click();
  const pws = card ? Array.from(card.querySelectorAll('input[type="password"]')) : [];
  for (const p of pws) { set.call(p, "s3cret"); p.dispatchEvent(new Event("input", { bubbles: true })); }
  const btn = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /create/i.test(b.textContent));
  btn && btn.click();
})()`);
await new Promise((r) => setTimeout(r, 800));
const createdEnc = await evalJs(`(() => {
  const row = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "created-secret.md");
  return { exists: !!row, locked: !!(row && row.querySelector(".row-lock")), unlocked: window.__unlocked.has && Array.from(window.__unlocked).length };
})()`);
if (!createdEnc.exists || !createdEnc.locked) {
  console.log("FAIL: creating an encrypted note lists it with a lock icon", JSON.stringify(createdEnc));
  fail++;
}

// ---- drag & drop media: file copied to .assets/, ![name](rel) inserted ----
await evalJs(`(() => {
  const el = Array.from(document.querySelectorAll("button.tree-item")).find(b => b.title === "plain.md");
  el && el.click();
  return !!el;
})()`);
await new Promise((r) => setTimeout(r, 1200));
const dropped = await evalJs(`(() => {
  const content = document.querySelector(".cm-content");
  if (!content) return { ok: false, why: "no editor" };
  const r = content.getBoundingClientRect();
  const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]);
  const file = new File([bytes], "photo.png", { type: "image/png" });
  const dt = new DataTransfer();
  dt.items.add(file);
  const ev = new DragEvent("drop", {
    dataTransfer: dt, bubbles: true, cancelable: true,
    clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
  });
  content.dispatchEvent(ev);
  return { ok: true, prevented: ev.defaultPrevented };
})()`);
if (!dropped.ok) { console.log("FAIL: could not dispatch the media drop", JSON.stringify(dropped)); fail++; }
else if (!dropped.prevented) { console.log("FAIL: media drop was not handled (defaultPrevented)"); fail++; }
await new Promise((r) => setTimeout(r, 800)); // the import runs async
const dropRes = await evalJs(`(() => ({
  stored: window.__storedMedia || {},
  doc: document.querySelector(".cm-content")?.snView?.state.doc.toString() ?? null,
  imgWrap: !!document.querySelector(".md-img-wrap"),
}))()`);
console.log("media drop:", JSON.stringify(dropRes));
if (!dropRes.stored["photo.png"]) {
  console.log("FAIL: dropped file was not stored in .assets/", JSON.stringify(dropRes.stored));
  fail++;
}
if (!dropRes.doc || !dropRes.doc.includes("![photo](.assets/photo.png)")) {
  console.log("FAIL: ![name](.assets/...) not inserted at the drop point", JSON.stringify(dropRes.doc));
  fail++;
}
if (!dropRes.imgWrap) {
  console.log("FAIL: the inserted image is not rendered in live preview");
  fail++;
}
// a non-media file must be ignored by the import handler (CM's text drop
// would still apply, so the doc just must not gain an .assets/ reference)
const droppedTxt = await evalJs(`(() => {
  const content = document.querySelector(".cm-content");
  if (!content) return { ok: false };
  const r = content.getBoundingClientRect();
  const file = new File([new TextEncoder().encode("hello")], "readme.txt", { type: "text/plain" });
  const dt = new DataTransfer();
  dt.items.add(file);
  const ev = new DragEvent("drop", {
    dataTransfer: dt, bubbles: true, cancelable: true,
    clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
  });
  content.dispatchEvent(ev);
  return { ok: true };
})()`);
await new Promise((r) => setTimeout(r, 500));
const dropTxtRes = await evalJs(`(() => ({
  stored: Object.keys(window.__storedMedia || {}).filter((n) => n.endsWith(".txt")),
}))()`);
if (dropTxtRes.stored.length) {
  console.log("FAIL: a non-media file was imported into .assets/", JSON.stringify(dropTxtRes));
  fail++;
}

// ---- URL paste / URL drop: bare link becomes a [host](url) card ----
const urlPasted = await evalJs(`(() => {
  const content = document.querySelector(".cm-content");
  if (!content) return false;
  const dt = new DataTransfer();
  dt.setData("text/plain", "https://spidernotes.app/docs");
  const ev = new ClipboardEvent("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(ev, "clipboardData", { value: dt });
  content.dispatchEvent(ev);
  return ev.defaultPrevented;
})()`);
await new Promise((r) => setTimeout(r, 400));
const urlPasteRes = await evalJs(`(() => ({
  doc: document.querySelector(".cm-content")?.snView?.state.doc.toString() ?? null,
  cards: Array.from(document.querySelectorAll(".md-link-card")).map(c => c.getAttribute("data-url")),
}))()`);
console.log("url paste:", JSON.stringify(urlPasteRes));
if (!urlPasted || !urlPasteRes.doc || !urlPasteRes.doc.includes("[spidernotes.app](https://spidernotes.app/docs)")) {
  console.log("FAIL: pasting a bare URL inserts a [host](url) link", JSON.stringify(urlPasteRes));
  fail++;
}
if (!urlPasteRes.cards.includes("https://spidernotes.app/docs")) {
  console.log("FAIL: the pasted URL renders as a link card", JSON.stringify(urlPasteRes.cards));
  fail++;
}
const urlDropped = await evalJs(`(() => {
  const content = document.querySelector(".cm-content");
  if (!content) return false;
  const r = content.getBoundingClientRect();
  const dt = new DataTransfer();
  dt.setData("text/plain", "https://example.org/x");
  const ev = new DragEvent("drop", {
    dataTransfer: dt, bubbles: true, cancelable: true,
    clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
  });
  content.dispatchEvent(ev);
  return ev.defaultPrevented;
})()`);
await new Promise((r) => setTimeout(r, 400));
const urlDropRes = await evalJs(`(() => ({
  doc: document.querySelector(".cm-content")?.snView?.state.doc.toString() ?? null,
}))()`);
if (!urlDropped || !urlDropRes.doc || !urlDropRes.doc.includes("[example.org](https://example.org/x)")) {
  console.log("FAIL: dropping a URL inserts a [host](url) link", JSON.stringify(urlDropRes));
  fail++;
}

// ---- storage: the assets line in settings + the clean-up dialog ----
const gear = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll("button")).find(b => b.title === "Network & log");
  b && b.click();
  return !!b;
})()`);
await new Promise((r) => setTimeout(r, 600));
const storage = await evalJs(`(() => ({
  line: Array.from(document.querySelectorAll(".muted.small")).find(el => /assets:/.test(el.textContent || ""))?.textContent || null,
  btn: !!Array.from(document.querySelectorAll("button")).find(b => /clean up unused assets/i.test(b.textContent || "")),
}))()`);
console.log("storage section:", JSON.stringify(storage));
if (!gear || !storage.line || !/1 unused/.test(storage.line) || !/3\.4 KB/.test(storage.line)) {
  console.log("FAIL: settings shows the assets line with the orphan count", JSON.stringify(storage));
  fail++;
}
if (!storage.btn) {
  console.log("FAIL: the 'Clean up unused assets…' button is missing");
  fail++;
}
await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll("button")).find(b => /clean up unused assets/i.test(b.textContent || ""));
  b && b.click();
})()`);
await new Promise((r) => setTimeout(r, 400));
const cleanDlg = await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /clean up/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const rows = card ? Array.from(card.querySelectorAll(".asset-row")).map(r => r.textContent) : [];
  const del = card ? Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /delete/i.test(b.textContent)) : null;
  return {
    hasDlg: !!dlg,
    rows,
    warn: card ? /could not be checked/i.test(card.textContent) : false,
    delText: del ? del.textContent : null,
  };
})()`);
console.log("clean-up dialog:", JSON.stringify(cleanDlg));
if (!cleanDlg.hasDlg || cleanDlg.rows.length !== 1 || !cleanDlg.rows[0].includes("old.gif")) {
  console.log("FAIL: the clean-up dialog lists exactly the orphan file", JSON.stringify(cleanDlg));
  fail++;
}
if (!cleanDlg.warn) {
  console.log("FAIL: the dialog warns about unchecked encrypted notes");
  fail++;
}
if (!cleanDlg.delText || !/Delete 1/.test(cleanDlg.delText)) {
  console.log("FAIL: the delete button shows the count", JSON.stringify(cleanDlg.delText));
  fail++;
}
await evalJs(`(() => {
  const dlg = Array.from(document.querySelectorAll(".dlg-title")).find(t => /clean up/i.test(t.textContent));
  const card = dlg && dlg.closest(".dlg");
  const del = card && Array.from(card.querySelectorAll(".dlg-actions button")).find(b => /delete/i.test(b.textContent));
  del && del.click();
})()`);
await new Promise((r) => setTimeout(r, 400));
const cleanRes = await evalJs(`(() => ({
  deleted: window.__deletedAssets || null,
  dlgGone: !Array.from(document.querySelectorAll(".dlg-title")).find(t => /clean up/i.test(t.textContent)),
}))()`);
if (!Array.isArray(cleanRes.deleted) || cleanRes.deleted.join() !== ".assets/old.gif" || !cleanRes.dlgGone) {
  console.log("FAIL: confirming deletes exactly the listed orphans and closes the dialog", JSON.stringify(cleanRes));
  fail++;
}

console.log("\n=== EXCEPTIONS ===");
if (exceptions.length) {
  fail++;
  exceptions.slice(0, 10).forEach((l) => console.log("  " + l.slice(0, 400)));
} else {
  console.log("  (none)");
}

ws.close();
server.close();
chrome.kill("SIGKILL");
console.log(fail ? `\n${fail} FAILURES` : "all browser checks pass");
process.exit(fail ? 1 : 0);
