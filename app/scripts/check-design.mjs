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
const LONG = "x".repeat(400);
const hlc = { ms: 1, counter: 0, device: "local" };
const notesList = [
  { uuid: "long0000-0000-0000-0000-000000000001", path: "long.md", hlc, hash: "", deleted: false },
  { uuid: "fold0000-0000-0000-0000-000000000002", path: "work/idea.md", hlc, hash: "", deleted: false },
];

let mockSource = "";
const mock = (extra) => `
window.__invokeLog = [];
window.__invokeArgs = [];
window.__mockNotesList = ${JSON.stringify(notesList)};
const hlc = ${JSON.stringify(hlc)};
${extra || ""}
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    window.__invokeLog.push(cmd);
    switch (cmd) {
      case "vault_status": return null;
      case "default_vault_path": return "/mock/vault";
      case "open_vault":
        return { path: "/mock/vault", p2p_port: 8788, device_id: "localdev", device_name: "mock", notes: window.__mockNotesList, peers: [], folders: ["work"] };
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
      case "p2p_settings": return { autosync_secs: 30, p2p_port: 8788, p2p_port_setting: 8788, p2p_running: true };
      case "server_sync_status": return { status: "server sync is off" };
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
check("font size default 14px (body + editor)", ed.bodyFontSize === "14px" && ed.contentFontSize === "14px", `body=${ed.bodyFontSize} content=${ed.contentFontSize}`);

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
const toRgb = (hex) => hex && hex.startsWith("#") && hex.length === 7
  ? `rgb(${parseInt(hex.slice(1,3),16)}, ${parseInt(hex.slice(3,5),16)}, ${parseInt(hex.slice(5,7),16)})`
  : null;
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
// the slider scales the NOTE TEXT only; the UI chrome keeps 14px
check("Visual: font size slider scales the note text only (body stays 14px)",
  fs16.body === "14px" && fs16.content === "16px" && fs16.gutter === "16px" && fs16.val === "16" && fs16.ls === "16",
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
check("persisted: note font size 17px (body stays 14px)", p2.bodyFs === "14px" && p2.contentFs === "17px", `body=${p2.bodyFs} content=${p2.contentFs}`);
check("persisted: line numbers stay off", !p2.gutter);

// ============ scenario 3: mobile full-screen sidebar, swipes only ============
await send("Emulation.setDeviceMetricsOverride", { width: 400, height: 800, deviceScaleFactor: 1, mobile: true });
await loadWith(`
localStorage.removeItem("sn.showSidebar");
localStorage.removeItem("sn.showNet"); // scenario 1 left the panel open
`, URL);
const railHidden = await evalJs(`getComputedStyle(document.querySelector(".side-rail")).display === "none"`);
check("mobile: the rail button is hidden", railHidden);
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
// close with a left swipe inside the list — the overlay slides off to the left
const swipeClose = await evalJs(`(() => {
  const target = document.querySelector(".sidebar .tree-item") || document.querySelector(".side-top");
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

await send("Emulation.clearDeviceMetricsOverride");
chrome.kill();
server.close();
console.log(fail ? `\n${fail} FAILURES` : "\nALL OK");
process.exit(fail ? 1 : 0);
