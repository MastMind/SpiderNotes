// Browser-level check for the design batch:
//  1. editor wraps long lines (no horizontal scroll, always full width)
//  2. monospace fonts (body + editor)
//  3. folder icons = theme-aware SVGs (no emoji), open/closed via CSS
//  4. line numbers gutter on/off (sn.lineNumbers)
//  5. themes (sn.theme) + font size (sn.fontSize) + Design section in the panel
//  6. mobile: full-screen notes list overlay + close button
//
// Runs the real bundle (vite build) in headless Chromium with a mocked
// Tauri IPC (injected into index.html by the local server).
//
// Requires: chromium (or chrome) on PATH and a fresh `vite build`.
import { readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { extname } from "node:path";
import path from "node:path";
import { createHash } from "node:crypto";

// Run: node app/scripts/check-design.mjs
import { fileURLToPath } from "node:url";

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DIST = path.join(appDir, "dist");
try {
  readFileSync(path.join(DIST, "index.html"));
} catch {
  console.error("dist/ not found — run `vite build` first");
  process.exit(2);
}
const PKG_VERSION = JSON.parse(
  readFileSync(path.join(appDir, "package.json"), "utf8")
).version;
const LONG = "x".repeat(400);
const hlc = { ms: 1, counter: 0, device: "local" };
// fixed "last sync" moment for the footer check (14.11.2023 22:13 UTC)
const LAST_SYNC = 1700000000;
const mockPeer = {
  name: "peer",
  device_id: "p2p00000-0000-0000-0000-000000000001",
  x25519_pub: [],
  address: "127.0.0.1",
  port: 8788,
};
// same comparison-code derivation as the app (PeersPanel.useCodes):
// first 5 SHA-256 bytes -> 6 Crockford-base32 chars
const CODE_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const codeFor = (id) => {
  const b = createHash("sha256").update(id).digest().subarray(0, 5);
  let bits = 0;
  for (const x of b) bits = (bits << 8) | x;
  let out = "";
  for (let g = 5; g >= 0; g--) out += CODE_ALPHABET[(bits >> (g * 5)) & 31];
  return out;
};
// "#rrggbb" -> "rgb(r, g, b)" (null when not a 7-char hex color)
const toRgb = (hex) => hex && hex.startsWith("#") && hex.length === 7
  ? `rgb(${parseInt(hex.slice(1,3),16)}, ${parseInt(hex.slice(3,5),16)}, ${parseInt(hex.slice(5,7),16)})`
  : null;
// same "when" formatting as the app (NotesTree.formatWhen)
const fmtWhen = (ms) => {
  const d = new Date(ms), now = new Date();
  if (d.toDateString() === now.toDateString())
    return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
  const opts = d.getFullYear() === now.getFullYear()
    ? { day: "2-digit", month: "2-digit" }
    : { day: "2-digit", month: "2-digit", year: "2-digit" };
  return d.toLocaleDateString("ru-RU", opts);
};
const notesList = [
  // title/preview as computed by the backend from the note bodies below
  // (long.md has no heading -> first line is the title; idea.md is one line)
  { uuid: "long0000-0000-0000-0000-000000000001", path: "long.md", hlc, hash: "", deleted: false, title: "line one", preview: "x".repeat(80) + "…" },
  { uuid: "fold0000-0000-0000-0000-000000000002", path: "work/idea.md", hlc, hash: "", deleted: false, title: "body of work/idea.md", preview: "" },
];

// a pending sync conflict on long.md (the row gets a warning icon; the
 // panel offers both versions; resolve empties the list)
const mockConflict = {
  id: "conflict-1",
  note_uuid: "long0000-0000-0000-0000-000000000001",
  note_path: "long.md",
  peer_name: "peer",
  peer_id: "p2p00000-0000",
  created_ms: LAST_SYNC * 1000,
  our_hash: "o",
  their_hash: "t",
  their_hlc: { ms: LAST_SYNC * 1000, counter: 0, device: "p2p00000-0000" },
  their_title: "peer title",
  their_preview: "",
};
const THEIR_CONTENT = "PEER VERSION of long.md";

let mockSource = "";
const mock = (extra) => `
window.__invokeLog = [];
window.__invokeArgs = [];
window.__mockNotesList = ${JSON.stringify(notesList)};
window.__mockConflicts = [${JSON.stringify(mockConflict)}];
const hlc = ${JSON.stringify(hlc)};
${extra || ""}
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    window.__invokeLog.push(cmd);
    window.__invokeArgs.push([cmd, args]);
    switch (cmd) {
      case "vault_status": return null;
      case "default_vault_path": return "/mock/vault";
      case "plugin:dialog|open": return "/mock/picked-vault";
      case "set_device_name": return { id: "localdev", name: args.name };
      case "open_vault":
        return { path: "/mock/vault", p2p_port: 8788, device_id: "localdev", device_name: "mock", notes: window.__mockNotesList, peers: [${JSON.stringify(mockPeer)}], folders: ["work"] };
      case "list_notes": return window.__mockNotesList;
      case "list_folders": return ["work"];
      case "read_note": {
        const n = window.__mockNotesList.find((x) => x.uuid === args.uuid);
        if (!n) throw new Error("unknown note " + args.uuid);
        const content = n.path === "long.md"
          ? "line one\\n" + ${JSON.stringify(LONG)} + "\\nline three\\n"
          : "body of " + n.path;
        return { uuid: n.uuid, path: n.path, content, hlc, deleted: false };
      }
      case "save_note": return { uuid: "x", path: args.path, hlc, hash: "", deleted: false };
      case "search_notes": return [];
      case "backlinks": return [];
      case "all_tags": return [];
      case "notes_with_tag": return [];
      case "device_info": return { id: "localdev", name: "mock" };
      case "list_peers": return [];
      case "list_trusted": return [];
      case "p2p_settings": return { autosync_secs: 30, p2p_port: 8788, p2p_port_setting: 8788, p2p_running: true, last_sync: ${LAST_SYNC} };
       case "server_sync_status": return { enabled: false, connected: false, status: "server sync is off" };
       case "sync_all": return [];
       case "list_conflicts": return window.__mockConflicts;
       case "conflict_content": return ${JSON.stringify(THEIR_CONTENT)};
       case "resolve_conflict": window.__mockConflicts = []; return null;
       default: return null;
    }
  },
  transformCallback: (cb) => {
    const id = Math.floor(Math.random() * 1e9);
    window.__cbs = window.__cbs || {};
    window.__cbs[id] = cb;
    return id;
  },
  unregisterCallback: (id) => { delete window.__cbs[id]; },
  convertFileSrc: (p) => "asset://" + p,
};
// @tauri-apps/api/event's unlisten() calls into the event plugin's internals
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
`;

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2" };
const BINARY = new Set([".woff2", ".woff", ".png"]);
const server = createServer((req, res) => {
  let p = req.url.split("?")[0];
  if (p === "/") p = "/index.html";
  try {
    // binary assets must be served as buffers (utf8 decoding corrupts them)
    let body = readFileSync(path.join(DIST, p), BINARY.has(extname(p)) ? undefined : "utf8");
    if (p === "/index.html" && mockSource) {
      // inline classic script BEFORE the module bundle: runs first, always
      body = body.replace(
        '<script type="module"',
        `<script>${mockSource.replace(/<\/script>/g, "<\\/script>")}</script>\n    <script type="module"`,
      );
    }
    res.writeHead(200, { "Content-Type": MIME[extname(p)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});
await new Promise((r) => server.listen(8129, r));

// kill leftovers from previous runs (a zombie on the debug port would
// steal the CDP session)
spawn("pkill", ["-f", "remote-debugging-port=9229"], { stdio: "ignore" });
await new Promise((r) => setTimeout(r, 800));

const chrome = spawn("chromium", [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  "--remote-debugging-port=9229", "--window-size=1280,800", "about:blank",
], { stdio: ["ignore", "pipe", "pipe"] });
chrome.stderr.on("data", () => {});

let targets = null;
for (let i = 0; i < 40; i++) {
  await new Promise((r) => setTimeout(r, 500));
  try {
    targets = await (await fetch("http://127.0.0.1:9229/json")).json();
    if (targets.some((t) => t.type === "page")) break;
  } catch {}
}
if (!targets) { console.error("chromium did not start"); process.exit(2); }
const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = () => j(new Error("ws error")); });

let idc = 0;
const pending = new Map();
const exceptions = [];
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  else if (msg.method === "Runtime.exceptionThrown") {
    const d = msg.params.exceptionDetails;
    exceptions.push(`${d.exception?.description || d.text}`);
    console.log("PAGE-EXC:", (d.exception?.description || d.text).slice(0, 300));
  } else if (msg.method === "Runtime.consoleAPICalled" && msg.params.type === "error") {
    console.log("PAGE-CONSOLE-ERR:", JSON.stringify(msg.params.args).slice(0, 300));
  }
};
const send = (method, params = {}) =>
  new Promise((res) => {
    const id = ++idc;
    pending.set(id, res);
    ws.send(JSON.stringify({ id, method, params }));
  });

const evalJs = (expression) =>
  send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }).then((m) => {
    if (m.result?.exceptionDetails)
      console.log("EVAL-EXC:", (m.result.exceptionDetails.exception?.description || m.result.exceptionDetails.text).slice(0, 200));
    return m.result?.result?.value;
  });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let fail = 0;
const check = (label, cond, extra = "") => {
  console.log(`${cond ? "ok  " : "FAIL"} ${label}${extra ? ` — ${extra}` : ""}`);
  if (!cond) fail++;
};

async function loadWith(extraMock, url) {
  mockSource = mock(extraMock);
  await send("Page.navigate", { url });
  await sleep(2500);
  await evalJs(`(() => {
    const b = Array.from(document.querySelectorAll("button")).find(b => /open.*vault/i.test(b.textContent));
    if (b) b.click();
    return b ? "clicked" : "no button";
  })()`);
  await sleep(1200);
}

const URL = "http://127.0.0.1:8129/";

// ============ scenario 1: defaults (dark, 14px, line numbers ON) ============
await loadWith(null, URL);

// ---- vault picker: "Change vault…" queues a pending vault (applied at
// the next launch); first launch offers a native folder picker ----
await evalJs(`(() => {
  const gear = Array.from(document.querySelectorAll("button")).find(b => b.title === "Network & log");
  gear.click();
  return "gear";
})()`);
await sleep(300);
const cvBtn = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".panel button")).find(x => x.textContent.trim() === "Change vault…");
  b && b.click();
  return !!b;
})()`);
await sleep(300);
const pending1 = await evalJs(`(() => ({
  banner: document.querySelector(".pending-vault") ? document.querySelector(".pending-vault").textContent : null,
  ls: localStorage.getItem("sn.pendingVaultPath"),
}))()`);
check("settings: 'Change vault…' queues a pending vault + banner",
  cvBtn === true && pending1.ls === "/mock/picked-vault" && pending1.banner && pending1.banner.includes("/mock/picked-vault"),
  JSON.stringify(pending1));
// relaunch: the pending vault opens automatically (no welcome screen,
// banner gone, sn.vaultPath updated, pending cleared)
await send("Page.navigate", { url: URL });
await sleep(3000);
const pending2 = await evalJs(`(() => ({
  welcome: !!document.querySelector(".welcome"),
  banner: !!document.querySelector(".pending-vault"),
  vault: localStorage.getItem("sn.vaultPath"),
  pending: localStorage.getItem("sn.pendingVaultPath"),
  tree: !!document.querySelector(".tree-item"),
}))()`);
check("relaunch: pending vault auto-opens, welcome + banner gone",
  pending2.welcome === false && pending2.banner === false && pending2.vault === "/mock/picked-vault" && pending2.pending === null && pending2.tree,
  JSON.stringify(pending2));
// fresh first launch (no saved path, panel closed): the folder picker
// fills the input
await evalJs(`localStorage.removeItem("sn.vaultPath"); localStorage.removeItem("sn.showNet")`);
await send("Page.navigate", { url: URL });
await sleep(3000);
const pick = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".welcome-card button")).find(x => x.textContent.trim() === "Choose folder…");
  b && b.click();
  return !!b;
})()`);
await sleep(300);
const picked = await evalJs(`(() => {
  const el = document.querySelector(".welcome-card input");
  if (!el) return null;
  return { tag: el.tagName, type: el.type, value: el.value, ctor: el.constructor ? el.constructor.name : "?" };
})()`);
check("first launch: 'Choose folder…' fills the vault path field",
  pick === true && picked && picked.value === "/mock/picked-vault", `pick=${pick} ${JSON.stringify(picked)}`);
// open it to restore the state for the remaining scenario 1 checks
await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".welcome-card button")).find(x => /open.*vault/i.test(x.textContent));
  b && b.click();
  return !!b;
})()`);
await sleep(1500);

await evalJs(`(() => {
  document.querySelector('button.tree-item[title="long.md"]').click();
  return "opened";
})()`);
await sleep(1000);

const ed = await evalJs(`(() => {
  const sc = document.querySelector(".cm-scroller");
  const content = document.querySelector(".cm-content");
  const gutter = document.querySelector(".cm-lineNumbers");
  const bodyFont = getComputedStyle(document.body).fontFamily;
  const contentFont = getComputedStyle(content).fontFamily;
  const editorEl = document.querySelector(".editor");
  const mainEl = document.querySelector(".main-content");
  return {
    scrollWidth: sc.scrollWidth,
    clientWidth: sc.clientWidth,
    wraps: sc.scrollWidth <= sc.clientWidth + 2,
    bodyFont,
    contentFont,
    hasGutter: !!gutter,
    editorWidth: editorEl.getBoundingClientRect().width,
    mainWidth: mainEl.getBoundingClientRect().width,
    contentFontSize: getComputedStyle(content).fontSize,
    bodyFontSize: getComputedStyle(document.body).fontSize,
  };
})()`);
if (!ed || typeof ed !== "object") {
  const html = await evalJs(`document.body ? document.body.innerHTML.slice(0, 600) : "no body"`);
  const tree = await evalJs(`Array.from(document.querySelectorAll("button.tree-item")).map(b => b.title).join(",") || "no tree items"`);
  console.log("BODY-DUMP:", html, "TREE:", tree);
  chrome.kill(); server.close(); process.exit(1);
}
check("editor: long line wraps (no horizontal overflow)", ed.wraps, `scrollWidth=${ed.scrollWidth} clientWidth=${ed.clientWidth}`);
check("editor: full width of the main area", Math.abs(ed.editorWidth - ed.mainWidth) < 2, `editor=${ed.editorWidth} main=${ed.mainWidth}`);
check("body font is monospace", /monospace/.test(ed.bodyFont), ed.bodyFont);
check("editor font is monospace", /monospace/.test(ed.contentFont), ed.contentFont);
check("line numbers gutter ON by default", ed.hasGutter);
check("font size default (body 15px chrome, note 14px)", ed.bodyFontSize === "15px" && ed.contentFontSize === "14px", `body=${ed.bodyFontSize} content=${ed.contentFontSize}`);

// Montserrat on the UI chrome (bundled woff2, offline), monospace content
const fonts = await evalJs(`document.fonts.ready.then(() => ({
  brand: getComputedStyle(document.querySelector(".brand")).fontFamily,
  loaded400: document.fonts.check("400 14px Montserrat"),
  loaded600: document.fonts.check("600 14px Montserrat"),
  editor: getComputedStyle(document.querySelector(".cm-content")).fontFamily,
  tree: getComputedStyle(document.querySelector("button.tree-item")).fontFamily,
}))`);
check("fonts: brand uses Montserrat", /Montserrat/.test(fonts.brand), fonts.brand);
check("fonts: Montserrat loaded (400 + 600)", fonts.loaded400 && fonts.loaded600, `400=${fonts.loaded400} 600=${fonts.loaded600}`);
check("fonts: editor + notes tree stay monospace",
  /monospace/.test(fonts.editor) && !/Montserrat/.test(fonts.editor) &&
  /monospace/.test(fonts.tree) && !/Montserrat/.test(fonts.tree),
  `editor=${fonts.editor} tree=${fonts.tree}`);

// ============ save on hide/exit: pending edits must land on disk ============
// The autosave debounce is 2s; pagehide, visibilitychange->hidden (app
// backgrounded) and the window-close handshake (sn-before-close ->
// force_close) must flush the pending edit immediately. long.md is open;
// the cursor sits at pos 0, so each insertText prefixes the content.
await evalJs(`(() => { document.querySelector(".cm-content").focus(); return "focused"; })()`);
await send("Input.insertText", { text: " pagehide" });
await sleep(300);
const ph0 = await evalJs(`window.__invokeLog.filter((c) => c === "save_note").length`);
check("save on hide: nothing saved before the 2s debounce elapses", ph0 === 0, `saves=${ph0}`);
await evalJs(`window.dispatchEvent(new Event("pagehide"))`);
await sleep(400);
const ph1 = await evalJs(`(() => {
  const args = window.__invokeArgs.filter(([c]) => c === "save_note").map(([, a]) => a.content);
  return { saves: window.__invokeLog.filter((c) => c === "save_note").length, last: args[args.length - 1] || "" };
})()`);
check("save on hide: pagehide flushes the pending edit",
  ph1.saves === 1 && ph1.last.startsWith(" pagehide"), JSON.stringify(ph1));

// window close: the Rust side intercepts the close and emits sn-before-close;
// the frontend flushes and confirms with force_close (simulate the emit)
await send("Input.insertText", { text: " closing" });
await sleep(200);
const cl0 = await evalJs(`(() => {
  const hit = window.__invokeArgs.find(([c, a]) => c === "plugin:event|listen" && a.event === "sn-before-close");
  if (!hit) return "no listener";
  window.__cbs[hit[1].handler]({ event: "sn-before-close", id: hit[1].handler, payload: null });
  return "emitted";
})()`);
await sleep(500);
const cl1 = await evalJs(`(() => {
  const args = window.__invokeArgs.filter(([c]) => c === "save_note").map(([, a]) => a.content);
  return {
    saves: window.__invokeLog.filter((c) => c === "save_note").length,
    forced: window.__invokeLog.includes("force_close"),
    last: args[args.length - 1] || "",
  };
})()`);
check("save on exit: sn-before-close flushes the pending edit",
  cl0 === "emitted" && cl1.saves === 2 && cl1.last.startsWith(" pagehide closing"),
  `${cl0} ${JSON.stringify(cl1)}`);
check("save on exit: force_close confirms the window destroy", cl1.forced);

// app backgrounded (Android freezes the webview): visibilitychange -> hidden
await send("Input.insertText", { text: " hidden" });
await sleep(200);
await evalJs(`(() => {
  const orig = Object.getOwnPropertyDescriptor(Document.prototype, "visibilityState");
  Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
  Object.defineProperty(document, "visibilityState", orig);
  return "hidden";
})()`);
await sleep(400);
const hv = await evalJs(`(() => {
  const args = window.__invokeArgs.filter(([c]) => c === "save_note").map(([, a]) => a.content);
  return { saves: window.__invokeLog.filter((c) => c === "save_note").length, last: args[args.length - 1] || "" };
})()`);
check("save on hide: visibilitychange->hidden flushes the pending edit",
  hv.saves === 3 && hv.last.startsWith(" pagehide closing hidden"), JSON.stringify(hv));

// sync footer: connection mode + last sync time + manual sync button
const sf0 = await evalJs(`(() => {
  const f = document.querySelector(".sync-foot");
  if (!f) return null;
  const btn = f.querySelector("button.icon-btn");
  return {
    text: f.querySelector(".sync-text")?.textContent.trim(),
    hasIco: !!f.querySelector(".sync-ico"),
    btnEnabled: btn ? !btn.disabled : null,
  };
})()`);
check("sync footer: local network mode + last sync time",
  sf0 !== null && sf0.hasIco && sf0.text === `Local network · ${fmtWhen(LAST_SYNC * 1000)}`,
  JSON.stringify(sf0));
check("sync footer: sync button enabled when peers are present", sf0?.btnEnabled === true);
await evalJs(`(() => { document.querySelector(".sync-foot button.icon-btn").click(); return "clicked"; })()`);
await sleep(300);
const sf1 = await evalJs(`window.__invokeLog.includes("sync_all")`);
check("sync footer: sync button triggers sync_all", sf1 === true);

// sidebar slide: rail closes it (slides off-screen, editor widens) and
// opens it back (slides in, editor returns to its width)
const rail0 = await evalJs(`(() => {
  const m = document.querySelector(".main-content").getBoundingClientRect().width;
  document.querySelector(".side-rail").click();
  return m;
})()`);
await sleep(450);
const sbClosed = await evalJs(`(() => {
  const s = document.querySelector(".sidebar");
  const r = s.getBoundingClientRect();
  return {
    right: r.right,
    opacity: getComputedStyle(s).opacity,
    main: document.querySelector(".main-content").getBoundingClientRect().width,
  };
})()`);
check("sidebar: closing slides it off-screen and widens the editor",
  sbClosed.right <= 1 && sbClosed.opacity === "0" && sbClosed.main > rail0 + 200,
  JSON.stringify(sbClosed));
await evalJs(`(() => { document.querySelector(".side-rail").click(); return "x"; })()`);
await sleep(450);
const sbOpen = await evalJs(`(() => {
  const s = document.querySelector(".sidebar");
  const r = s.getBoundingClientRect();
  return {
    left: r.left,
    width: r.width,
    opacity: getComputedStyle(s).opacity,
    main: document.querySelector(".main-content").getBoundingClientRect().width,
  };
})()`);
check("sidebar: opening slides it back (editor width restored)",
  sbOpen.left === 0 && sbOpen.width > 200 && sbOpen.opacity === "1" && Math.abs(sbOpen.main - rail0) < 2,
  JSON.stringify(sbOpen));

// resizable sidebar: drag the divider (clamped 180..420, persisted,
// double-click resets)
const rhAt = () => evalJs(`(() => {
  const r = document.querySelector(".resize-handle").getBoundingClientRect();
  return { x: r.x + r.width / 2, y: 300 };
})()`);
const mouse = (type, x, y, clickCount = 1) =>
  send("Input.dispatchMouseEvent", { type, x, y, button: "left", buttons: type === "mouseMoved" ? 1 : undefined, clickCount });
const rh0 = await evalJs(`(() => ({
  cursor: getComputedStyle(document.querySelector(".resize-handle")).cursor,
  sbw: getComputedStyle(document.querySelector(".sidebar")).width,
}))()`);
check("resize: handle present with col-resize cursor, panel 250px",
  rh0.cursor === "col-resize" && rh0.sbw === "250px", JSON.stringify(rh0));
{
  const p = await rhAt();
  await mouse("mousePressed", p.x, p.y);
  await mouse("mouseMoved", p.x + 50, p.y);
  await mouse("mouseReleased", p.x + 50, p.y);
  await sleep(150);
}
const rh1 = await evalJs(`(() => ({
  sbw: getComputedStyle(document.querySelector(".sidebar")).width,
  ls: localStorage.getItem("sn.sidebarW"),
}))()`);
check("resize: dragging the divider widens the panel and persists",
  rh1.sbw === "300px" && rh1.ls === "300", JSON.stringify(rh1));
{
  const p = await rhAt();
  await mouse("mousePressed", p.x, p.y);
  await mouse("mouseMoved", 3000, p.y); // way past the max
  await mouse("mouseReleased", 3000, p.y);
  await sleep(150);
}
const rh2 = await evalJs(`getComputedStyle(document.querySelector(".sidebar")).width`);
check("resize: width is clamped to the 420px max", rh2 === "420px", rh2);
{
  const p = await rhAt();
  await mouse("mousePressed", p.x, p.y);
  await mouse("mouseReleased", p.x, p.y);
  await mouse("mousePressed", p.x, p.y, 2);
  await mouse("mouseReleased", p.x, p.y, 2);
  await sleep(150);
}
const rh3 = await evalJs(`(() => ({
   sbw: getComputedStyle(document.querySelector(".sidebar")).width,
   ls: localStorage.getItem("sn.sidebarW"),
 }))()`);
check("resize: double-click on the divider resets to 250px",
  rh3.sbw === "250px" && rh3.ls === "250", JSON.stringify(rh3));

// sync conflict: the note row carries a warning icon (warn color); it opens
// the conflict panel with BOTH versions; resolving keeps the chosen one
const cf0 = await evalJs(`(() => {
  const ico = document.querySelector('button.tree-item[title="long.md"] .row-conflict');
  return {
    has: !!ico,
    color: ico ? getComputedStyle(ico).color : null,
    warn: getComputedStyle(document.documentElement).getPropertyValue("--warn").trim(),
  };
})()`);
check("conflict: note row shows a warning icon in the warn color",
  cf0.has && toRgb(cf0.warn) !== null && cf0.color === toRgb(cf0.warn),
  JSON.stringify(cf0));
await evalJs(`(() => {
  document.querySelector('button.tree-item[title="long.md"] .row-conflict').click();
  return "x";
})()`);
await sleep(600);
const cf1 = await evalJs(`(() => {
  const card = document.querySelector(".conflict-card");
  if (!card) return null;
  return {
    head: card.querySelector("h2")?.textContent,
    chips: Array.from(card.querySelectorAll(".conflict-chip")).map((c) => c.textContent.trim()),
    pre: Array.from(card.querySelectorAll(".conflict-pre")).map((p) => p.textContent),
    btns: Array.from(card.querySelectorAll(".conflict-version > button")).map((b) => ({ t: b.textContent.trim(), dis: b.disabled })),
  };
})()`);
check("conflict: panel shows both versions (this device + peer)",
  cf1 !== null && cf1.head === "Sync conflict" &&
  JSON.stringify(cf1.chips) === JSON.stringify(["This device", "peer"]) &&
  cf1.pre.length === 2 && cf1.pre[0].startsWith("line one") &&
  cf1.pre[1] === "PEER VERSION of long.md",
  JSON.stringify(cf1));
check("conflict: each version has an enabled 'Keep this version' button",
  cf1?.btns.length === 2 &&
  cf1.btns.every((b) => b.t === "Keep this version" && b.dis === false));
await evalJs(`(() => {
  document.querySelectorAll(".conflict-version > button")[1].click();
  return "clicked";
})()`);
await sleep(700);
const cf2 = await evalJs(`(() => ({
  resolved: window.__invokeArgs.some(([c, a]) => c === "resolve_conflict" && a.keepTheirs === true),
  closed: !document.querySelector(".conflict-card"),
  iconGone: !document.querySelector('button.tree-item[title="long.md"] .row-conflict'),
}))()`);
check("conflict: 'keep peer version' calls resolve_conflict and closes the panel",
  cf2.resolved && cf2.closed, JSON.stringify(cf2));
check("conflict: row warning disappears after resolving", cf2.iconGone);

// folder icon
const fi = await evalJs(`(() => {
  const sum = document.querySelector("summary.tree-dir");
  if (!sum) return "no folder";
  const svg = sum.querySelector(".dir-icon svg");
  const text = sum.textContent;
  const closed = getComputedStyle(sum.querySelector(".di-closed")).display;
  const open = getComputedStyle(sum.querySelector(".di-open")).display;
  const details = sum.closest("details");
  return {
    hasSvg: !!svg,
    emoji: /📁|📂/.test(text),
    closedDisplay: closed,
    openDisplay: open,
    isOpen: details.open,
 color: getComputedStyle(sum.querySelector(".dir-icon")).color,
   accent: getComputedStyle(document.documentElement).getPropertyValue("--accent").trim(),
   fg: getComputedStyle(sum).color,
  };
})()`);
check("folder icon: SVG rendered, no emoji", fi.hasSvg && !fi.emoji, JSON.stringify(fi));
check("folder icon: open state shows open variant", fi.isOpen ? fi.openDisplay !== "none" && fi.closedDisplay === "none" : fi.closedDisplay !== "none", `open=${JSON.stringify(fi)}`);
check("folder icon: colored with the theme accent", toRgb(fi.accent) !== null && fi.color === toRgb(fi.accent) && fi.color !== fi.fg, `icon=${fi.color} accent=${fi.accent}`);

// Design section in the settings panel
const ds = await evalJs(`(() => {
  const gear = Array.from(document.querySelectorAll("button")).find(b => b.title === "Network & log");
  gear.click();
  return "gear clicked";
})()`);
await sleep(600);

// comparison codes: my code in "My device", peer code in the row + confirm box
const cc0 = await evalJs(`(() => ({
  self: document.querySelector(".self-code-val")?.textContent.trim(),
  peerRow: document.querySelector(".peer-code")?.textContent.trim(),
}))()`);
check("comparison code: my device shows a 6-char code",
  cc0.self !== undefined && cc0.self === codeFor("localdev"), JSON.stringify(cc0));
check("comparison code: peer row shows the peer's code",
  cc0.peerRow === "· " + codeFor(mockPeer.device_id), JSON.stringify(cc0));
await evalJs(`(() => {
  const t = Array.from(document.querySelectorAll("button.trust")).find(b => b.textContent.trim() === "Trust");
  if (t) t.click();
  return !!t;
})()`);
await sleep(300);
const cc1 = await evalJs(`(() => ({
  code: document.querySelector(".trust-code")?.textContent.trim(),
  id: document.querySelector(".trust-confirm .trust-id")?.textContent.trim(),
}))()`);
check("trust confirm: short code + full ID for comparison",
  cc1.code === codeFor(mockPeer.device_id) && cc1.id === mockPeer.device_id,
  JSON.stringify(cc1));
await evalJs(`(() => {
  const c = Array.from(document.querySelectorAll(".trust-confirm .peer-actions button")).find(b => b.textContent.trim() === "Cancel");
  if (c) c.click();
  return !!c;
})()`);
await sleep(200);
// device rename: pencil next to the name -> dialog -> set_device_name
const devPencil = await evalJs(`(() => {
  const b = document.querySelector(".selfdev .icon-btn");
  b && b.click();
  return !!b;
})()`);
await sleep(200);
const devDlg = await evalJs(`(() => {
  const d = document.querySelector(".dlg");
  return {
    open: !!d,
    title: d ? (d.querySelector(".dlg-title") ? d.querySelector(".dlg-title").textContent : null) : null,
    value: d && d.querySelector("input") ? d.querySelector("input").value : null,
  };
})()`);
check("device rename: dialog opens with the current name",
  devPencil === true && devDlg.open && devDlg.title === "Rename device" && devDlg.value === "mock",
  JSON.stringify(devDlg));
const devRenamed = await evalJs(`(() => {
  const input = document.querySelector(".dlg input");
  if (!input) return "no-input";
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  set.call(input, "Kitchen tablet");
  input.dispatchEvent(new Event("input", { bubbles: true }));
  const btn = Array.from(document.querySelectorAll(".dlg-actions button")).find(x => x.textContent.trim() === "Rename");
  btn && btn.click();
  return !!btn;
})()`);
await sleep(300);
const devAfter = await evalJs(`(() => ({
  name: document.querySelector(".selfdev .peer-name") ? document.querySelector(".selfdev .peer-name").textContent : null,
  dlg: !!document.querySelector(".dlg"),
  args: window.__invokeArgs.filter(a => a[0] === "set_device_name").map(a => a[1]),
}))()`);
check("device rename: set_device_name called + UI updated",
  devRenamed === true && devAfter.name === "Kitchen tablet" && devAfter.dlg === false && devAfter.args.length === 1 && devAfter.args[0].name === "Kitchen tablet",
  JSON.stringify(devAfter));
const d1 = await evalJs(`(() => {
  const btn = Array.from(document.querySelectorAll("button")).find(b => /visual settings/i.test(b.textContent));
  if (!btn) return { found: false };
  btn.click();
  return { found: true };
})()`);
await sleep(300);
const d2 = await evalJs(`(() => {
  const sels = Array.from(document.querySelectorAll(".design-body select.sel"));
  const ev = sels[0];
  const nf = sels[1];
  const sel = sels[2];
  const opts = sel ? Array.from(sel.options).map(o => o.value) : [];
  const range = document.querySelector(".design-body input[type=range]");
  const cb = document.querySelector(".design-body input[type=checkbox]");
  return {
    sectionVisible: !!document.querySelector(".design-body"),
    editorView: ev ? Array.from(ev.options).map(o => o.value) : [],
    noteFont: nf ? Array.from(nf.options).map(o => o.value) : [],
    themes: opts,
    hasFontSize: !!range,
    hasLineNumbers: !!cb,
    cbChecked: cb ? cb.checked : null,
  };
})()`);
check("Visual: button in the panel toggles the section", d1.found && d2.sectionVisible);
check("Visual: editor view select (preview/raw) present", JSON.stringify(d2.editorView) === JSON.stringify(["preview","raw"]), JSON.stringify(d2.editorView));
check("Visual: note font select (mono/sans) present", JSON.stringify(d2.noteFont) === JSON.stringify(["mono","sans"]), JSON.stringify(d2.noteFont));
check("Visual: theme select has all 5 themes", JSON.stringify(d2.themes) === JSON.stringify(["dark","light","midnight","forest","paper"]), JSON.stringify(d2.themes));
check("Visual: font size slider + line numbers controls present", d2.hasFontSize && d2.hasLineNumbers);
check("Visual: line numbers checkbox reflects state (on)", d2.cbChecked === true);

// note font select: switch to Montserrat, the editor content follows
await evalJs(`(() => {
  const nf = document.querySelectorAll(".design-body select.sel")[1];
  nf.value = "sans";
  nf.dispatchEvent(new Event("change", { bubbles: true }));
  return "set";
})()`);
await sleep(400);
const nf2 = await evalJs(`(() => ({
  data: document.documentElement.dataset.noteFont,
  ls: localStorage.getItem("sn.noteFont"),
  content: getComputedStyle(document.querySelector(".cm-content")).fontFamily,
  gutter: getComputedStyle(document.querySelector(".cm-gutters")).fontFamily,
}))()`);
check("Visual: note font select switches the editor to Montserrat",
  nf2.data === "sans" && nf2.ls === "sans" && /Montserrat/.test(nf2.content) && /Montserrat/.test(nf2.gutter),
  JSON.stringify(nf2));
await evalJs(`(() => {
  const nf = document.querySelectorAll(".design-body select.sel")[1];
  nf.value = "mono";
  nf.dispatchEvent(new Event("change", { bubbles: true }));
  return "set";
})()`);
await sleep(200);

// font size slider: applies immediately (no blur/Enter), persists.
// (React tracks the input value itself, so set it through the native
//  setter — a plain r.value = ... is seen as "unchanged".)
await evalJs(`(() => {
  const r = document.querySelector(".design-body input[type=range]");
  const desc = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value");
  desc.set.call(r, "16");
  r.dispatchEvent(new Event("input", { bubbles: true }));
  r.dispatchEvent(new Event("change", { bubbles: true }));
  return "set";
})()`);
await sleep(400);
const fs16 = await evalJs(`(() => ({
  body: getComputedStyle(document.body).fontSize,
  content: getComputedStyle(document.querySelector(".cm-content")).fontSize,
  gutter: getComputedStyle(document.querySelector(".cm-gutters")).fontSize,
  val: document.querySelector(".design-body .range-val").textContent,
  ls: localStorage.getItem("sn.fontSize"),
}))()`);
// the slider scales the NOTE TEXT only; the UI chrome keeps its base size
check("Visual: font size slider scales the note text only (body stays 15px)",
  fs16.body === "15px" && fs16.content === "16px" && fs16.gutter === "16px" && fs16.val === "16" && fs16.ls === "16",
  JSON.stringify(fs16));

// editor view select: switch to raw, persisted
await evalJs(`(() => {
  const ev = document.querySelectorAll(".design-body select.sel")[0];
  ev.value = "raw";
  ev.dispatchEvent(new Event("change", { bubbles: true }));
  return "set";
})()`);
await sleep(300);
const evs = await evalJs(`(() => ({ ls: localStorage.getItem("sn.live") }))()`);
check("Visual: editor view select persists raw mode", evs.ls === "0", JSON.stringify(evs));

// switch theme to midnight inside the panel, verify CSS vars
await evalJs(`(() => {
  const sel = document.querySelectorAll(".design-body select.sel")[2];
  sel.value = "midnight";
  sel.dispatchEvent(new Event("change", { bubbles: true }));
  return "set";
})()`);
await sleep(300);
const th = await evalJs(`(() => ({
  dataTheme: document.documentElement.dataset.theme,
  bg: getComputedStyle(document.body).backgroundColor,
  midnightBg: "rgb(13, 20, 33)",
  ls: localStorage.getItem("sn.theme"),
  editorDark: !!document.querySelector(".cm-editor.cm-theme-oneDark") || true,
}))()`);
check("theme: midnight applied (data-theme + CSS var + persisted)",
  th.dataTheme === "midnight" && th.bg === th.midnightBg && th.ls === "midnight",
  `data=${th.dataTheme} bg=${th.bg} ls=${th.ls}`);

// toggle line numbers off via the checkbox
await evalJs(`(() => {
  const cb = document.querySelector(".design-body input[type=checkbox]");
  cb.click();
  return "toggled";
})()`);
await sleep(600);
const ln = await evalJs(`(() => ({
  gutter: !!document.querySelector(".cm-lineNumbers"),
  ls: localStorage.getItem("sn.lineNumbers"),
}))()`);
check("line numbers: checkbox turns the gutter off", !ln.gutter && ln.ls === "0", JSON.stringify(ln));

// ============ scenario 2: persisted settings apply on load ============
await loadWith(`
localStorage.setItem("sn.fontSize", "17");
localStorage.setItem("sn.theme", "forest");
localStorage.setItem("sn.lineNumbers", "0");
`, URL);
const p = await evalJs(`(() => {
  document.querySelector('button.tree-item[title="long.md"]').click();
  return "opened";
})()`);
await sleep(800);
const p2 = await evalJs(`(() => ({
  dataTheme: document.documentElement.dataset.theme,
  bodyFs: getComputedStyle(document.body).fontSize,
  contentFs: getComputedStyle(document.querySelector(".cm-content")).fontSize,
  gutter: !!document.querySelector(".cm-lineNumbers"),
  fg: getComputedStyle(document.body).color,
}))()`);
check("persisted: theme forest applied on load", p2.dataTheme === "forest", p2.dataTheme);
check("persisted: note font size 17px (body stays 15px)", p2.bodyFs === "15px" && p2.contentFs === "17px", `body=${p2.bodyFs} content=${p2.contentFs}`);
check("persisted: line numbers stay off", !p2.gutter);

// ============ scenario 3: mobile full-screen sidebar, swipes only ============
await send("Emulation.setDeviceMetricsOverride", { width: 400, height: 800, deviceScaleFactor: 1, mobile: true });
await loadWith(`
localStorage.removeItem("sn.showSidebar");
localStorage.removeItem("sn.showNet"); // scenario 1 left the panel open
`, URL);
const railHidden = await evalJs(`(() => ({
  rail: getComputedStyle(document.querySelector(".side-rail")).display,
  handle: getComputedStyle(document.querySelector(".resize-handle")).display,
}))()`);
check("mobile: the rail button and the resize handle are hidden",
  railHidden.rail === "none" && railHidden.handle === "none", JSON.stringify(railHidden));
// open with a right swipe from the MIDDLE of the screen (the whole screen
// is the trigger area on phones)
const swipeOpen = await evalJs(`(() => {
  const target = document.elementFromPoint(200, 400) || document.body;
  const t1 = new Touch({ identifier: 21, target, clientX: 120, clientY: 400, pageX: 120, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 21, target, clientX: 320, clientY: 400, pageX: 320, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t2] }));
  return true;
})()`);
await sleep(600);
const mb2 = await evalJs(`(() => {
  const sb = document.querySelector(".sidebar");
  const r = sb.getBoundingClientRect();
  return {
    left: r.left, top: r.top,
    w: r.width, h: r.height,
    vw: window.innerWidth, vh: window.innerHeight,
    pos: getComputedStyle(sb).position,
    editorCovered: !!document.querySelector(".main-content") && r.width >= window.innerWidth - 1,
  };
})()`);
check("mobile: right swipe (anywhere) opens the full-screen overlay",
  swipeOpen === true && mb2.pos === "fixed" && mb2.w >= mb2.vw - 1 && mb2.h >= mb2.vh - 1 && mb2.left === 0 && mb2.top === 0,
  JSON.stringify(mb2));
// close with a left swipe inside the list (NOT on a note row — rows
// have their own left-swipe actions) — the overlay slides off to the left
const swipeClose = await evalJs(`(() => {
  const target = document.querySelector(".side-top") || document.querySelector(".sidebar .tree-dir");
  const t1 = new Touch({ identifier: 22, target, clientX: 300, clientY: 400, pageX: 300, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 22, target, clientX: 100, clientY: 400, pageX: 100, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t2] }));
  return true;
})()`);
await sleep(450);
const mb3 = await evalJs(`(() => {
  const s = document.querySelector(".sidebar");
  if (!s) return { offscreen: true };
  const r = s.getBoundingClientRect();
  return { offscreen: r.right <= 1, right: r.right };
})()`);
check("mobile: left swipe slides the overlay off-screen", swipeClose === true && mb3.offscreen, JSON.stringify(mb3));
// regression: with a note OPEN the editor covers the whole screen — a
// right swipe from inside the editor must still open the list
await evalJs(`(() => {
  document.querySelector('button.tree-item[title="long.md"]').click();
  return "opened";
})()`);
await sleep(800);
const regPre = await evalJs(`(() => {
  const sb = document.querySelector(".sidebar").getBoundingClientRect();
  const at = document.elementFromPoint(200, 400);
  return {
    closed: sb.right <= 1,
    editorAtPoint: !!(at && at.closest && at.closest(".cm-editor")),
  };
})()`);
const swipeOpen2 = await evalJs(`(() => {
  const target = document.elementFromPoint(200, 400) || document.body;
  const t1 = new Touch({ identifier: 23, target, clientX: 120, clientY: 400, pageX: 120, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 23, target, clientX: 320, clientY: 400, pageX: 320, pageY: 400 });
  target.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t2] }));
  return true;
})()`);
await sleep(600);
const regPost = await evalJs(`(() => {
  const r = document.querySelector(".sidebar").getBoundingClientRect();
  return { left: r.left, top: r.top };
})()`);
check("mobile: swipe opens the list even with a note open (editor covers the screen)",
  regPre.closed && regPre.editorAtPoint && swipeOpen2 === true && regPost.left === 0 && regPost.top === 0,
  `pre=${JSON.stringify(regPre)} post=${JSON.stringify(regPost)}`);
// settings panel: the version line shows <version>-alpha + build sha
await evalJs(`(() => {
  const gear = Array.from(document.querySelectorAll("button")).find(b => b.title === "Network & log");
  gear.click();
  return "gear";
})()`);
await sleep(300);
const panelVer = await evalJs(`(() => {
  const p = document.querySelector(".panel");
  if (!p) return { found: false };
  const line = Array.from(p.querySelectorAll(".muted")).find(d => d.textContent.includes("alpha · build"));
  return { found: !!line, text: line ? line.textContent : null };
})()`);
check("panel: version line shows <version>-alpha + build sha",
  panelVer.found && panelVer.text === `${PKG_VERSION}-alpha · build local`,
  JSON.stringify(panelVer));
// a right swipe on the panel (start + move + end, dx > 80) closes it,
// besides the X
const swipePanel = await evalJs(`(() => {
  const p = document.querySelector(".panel");
  if (!p) return "no-panel";
  const t1 = new Touch({ identifier: 31, target: p, clientX: 100, clientY: 400, pageX: 100, pageY: 400 });
  p.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 31, target: p, clientX: 180, clientY: 400, pageX: 180, pageY: 400 });
  p.dispatchEvent(new TouchEvent("touchmove", { bubbles: true, cancelable: true, touches: [t2], targetTouches: [t2], changedTouches: [t2] }));
  const t3 = new Touch({ identifier: 31, target: p, clientX: 220, clientY: 400, pageX: 220, pageY: 400 });
  p.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t3] }));
  return true;
})()`);
await sleep(450);
const panelGone = await evalJs(`(() => !document.querySelector(".panel"))()`);
check("panel: right swipe closes the settings panel", swipePanel === true && panelGone, `swipe=${swipePanel} gone=${panelGone}`);

await send("Emulation.clearDeviceMetricsOverride");
chrome.kill();
server.close();
console.log(fail ? `\n${fail} FAILURES` : "\nALL OK");
process.exit(fail ? 1 : 0);
