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
const notesList = notes.map(({ content, ...s }) => s);

const mock = `
window.__cbs = {};
window.__invokeLog = [];
window.__mockNotes = ${JSON.stringify(Object.fromEntries(notes.map((n) => [n.uuid, n])))};
window.__mockNotesList = ${JSON.stringify(notesList)};
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
        return { uuid: n.uuid, path: n.path, content: n.content, hlc, deleted: false };
      }
      case "save_note": return { uuid: "x", path: args.path, hlc, hash: "", deleted: false };
      case "delete_note": return { uuid: args.uuid, path: "?", hlc, hash: "", deleted: true };
      case "rename_note": return { uuid: args.uuid, path: args.newPath, hlc, hash: "", deleted: false };
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
