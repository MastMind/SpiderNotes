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

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };
const server = createServer((req, res) => {
  let p = req.url.split("?")[0];
  if (p === "/") p = "/index.html";
  try {
    let body = readFileSync(path.join(DIST, p), "utf8");
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
  const sel = document.querySelector(".design-body select.sel");
  const opts = sel ? Array.from(sel.options).map(o => o.value) : [];
  const num = document.querySelector(".design-body input.num");
  const cb = document.querySelector(".design-body input[type=checkbox]");
  return {
    sectionVisible: !!document.querySelector(".design-body"),
    themes: opts,
    hasFontSize: !!num,
    hasLineNumbers: !!cb,
    cbChecked: cb ? cb.checked : null,
  };
})()`);
check("Design: button in the panel toggles the section", d1.found && d2.sectionVisible);
check("Design: theme select has all 5 themes", JSON.stringify(d2.themes) === JSON.stringify(["dark","light","midnight","forest","paper"]), JSON.stringify(d2.themes));
check("Design: font size + line numbers controls present", d2.hasFontSize && d2.hasLineNumbers);
check("Design: line numbers checkbox reflects state (on)", d2.cbChecked === true);

// switch theme to midnight inside the panel, verify CSS vars
await evalJs(`(() => {
  const sel = document.querySelector(".design-body select.sel");
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
check("persisted: font size 17px on body and editor", p2.bodyFs === "17px" && p2.contentFs === "17px", `body=${p2.bodyFs} content=${p2.contentFs}`);
check("persisted: line numbers stay off", !p2.gutter);

// ============ scenario 3: mobile full-screen sidebar ============
await send("Emulation.setDeviceMetricsOverride", { width: 400, height: 800, deviceScaleFactor: 1, mobile: true });
await loadWith(`localStorage.removeItem("sn.showSidebar");`, URL);
const mb = await evalJs(`(() => {
  const rail = document.querySelector(".side-rail");
  rail.click();
  return "rail clicked";
})()`);
await sleep(600);
const mb2 = await evalJs(`(() => {
  const sb = document.querySelector(".sidebar");
  const r = sb.getBoundingClientRect();
  const closeBtn = document.querySelector(".close-sidebar");
  return {
    left: r.left, top: r.top,
    w: r.width, h: r.height,
    vw: window.innerWidth, vh: window.innerHeight,
    pos: getComputedStyle(sb).position,
    closeVisible: closeBtn ? getComputedStyle(closeBtn).display !== "none" : false,
    editorCovered: !!document.querySelector(".main-content") && r.width >= window.innerWidth - 1,
  };
})()`);
check("mobile: sidebar is a full-screen overlay",
  mb2.pos === "fixed" && mb2.w >= mb2.vw - 1 && mb2.h >= mb2.vh - 1 && mb2.left === 0 && mb2.top === 0,
  JSON.stringify(mb2));
check("mobile: close button visible", mb2.closeVisible);
// close it with the button — the overlay slides off to the left
await evalJs(`(() => { document.querySelector(".close-sidebar").click(); return "x"; })()`);
await sleep(450);
const mb3 = await evalJs(`(() => {
  const s = document.querySelector(".sidebar");
  if (!s) return { offscreen: true };
  const r = s.getBoundingClientRect();
  return { offscreen: r.right <= 1, right: r.right };
})()`);
check("mobile: close button slides the overlay off-screen", mb3.offscreen, JSON.stringify(mb3));

await send("Emulation.clearDeviceMetricsOverride");
chrome.kill();
server.close();
console.log(fail ? `\n${fail} FAILURES` : "\nALL OK");
process.exit(fail ? 1 : 0);
