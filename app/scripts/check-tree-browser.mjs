// Browser-level check for the notes tree: folders, hidden .md, drag&drop
// (HTML5 DnD), context menus (right-click + touch long-press) and the
// edge-swipe gestures that open/close the sidebar.
//
// Runs the real bundle (vite build) in headless Chromium with a mocked
// Tauri IPC, dispatches synthetic mouse/touch/drag events and asserts on
// the rendered DOM and the invoked commands.
//
// Requires: chromium (or chrome) on PATH and a fresh `vite build`.
// Run: node app/scripts/check-tree-browser.mjs

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

// --- mock vault: notes across folders + empty folders ---
const realNotes = {
  "root.md": "---\nspidernotes-id: n0000000-0000-0000-0000-000000000001\n---\n\n# root\n",
  "work/idea.md": "---\nspidernotes-id: n0000000-0000-0000-0000-000000000002\n---\n\n# idea\n",
  "work/meeting.md": "---\nspidernotes-id: n0000000-0000-0000-0000-000000000003\n---\n\n# meeting\n",
  "home/recipe.md": "---\nspidernotes-id: n0000000-0000-0000-0000-000000000004\n---\n\n# recipe\n",
};
const mockFolders = ["work", "home", "empty-folder"];
const hlc = { ms: 1, counter: 0, device: "local" };
const notesList = Object.entries(realNotes).map(([p, content]) => ({
  uuid: content.match(/spidernotes-id:\s*(\S+)/)?.[1] ?? p,
  path: p,
  hlc,
  hash: "",
  deleted: false,
}));

const mock = `
window.__invokeLog = [];
window.__invokeArgs = [];
window.__mockNotesList = ${JSON.stringify(notesList)};
window.__mockFolders = ${JSON.stringify(mockFolders)};
const hlc = ${JSON.stringify(hlc)};
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    window.__invokeLog.push(cmd);
    window.__invokeArgs.push({ cmd, args });
    switch (cmd) {
      case "vault_status": return null;
      case "default_vault_path": return "/mock/vault";
      case "open_vault":
        return { path: "/mock/vault", p2p_port: 8788, device_id: "localdev", device_name: "mock", notes: window.__mockNotesList, peers: [], folders: window.__mockFolders };
      case "list_notes": return window.__mockNotesList;
      case "list_folders": return window.__mockFolders;
      case "read_note": {
        const n = window.__mockNotesList.find((x) => x.uuid === args.uuid);
        if (!n) throw new Error("unknown note " + args.uuid);
        return { uuid: n.uuid, path: n.path, content: "body of " + n.path, hlc, deleted: false };
      }
      case "save_note": return { uuid: "x", path: args.path, hlc, hash: "", deleted: false };
      case "delete_note": return { uuid: args.uuid, path: "?", hlc, hash: "", deleted: true };
      case "rename_note": return { uuid: args.uuid, path: args.newPath, hlc, hash: "", deleted: false };
      case "create_folder": return args.path;
      case "rename_folder": return 0;
      case "delete_folder": return [];
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
      case "p2p_settings": return { autosync_secs: 30, p2p_port: 8788, p2p_port_setting: 8788, p2p_running: true };
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
    window.__cbs = window.__cbs || {};
    window.__cbs[id] = cb;
    return id;
  },
  unregisterCallback: (id) => { delete window.__cbs[id]; },
  convertFileSrc: (p) => "asset://" + p,
};
`;

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png" };
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
await new Promise((r) => server.listen(8125, r));

const chromeBin = process.env.CHROME_BIN || "chromium";
const chrome = spawn(chromeBin, [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  "--remote-debugging-port=9225", "--window-size=1280,800", "about:blank",
], { stdio: ["ignore", "pipe", "pipe"] });
chrome.stderr.on("data", () => {});

let targets = null;
for (let i = 0; i < 40; i++) {
  await new Promise((r) => setTimeout(r, 500));
  try {
    targets = await (await fetch("http://127.0.0.1:9225/json")).json();
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
await send("Page.navigate", { url: "http://127.0.0.1:8125/" });
await new Promise((r) => setTimeout(r, 3000));

const evalJs = (expression) =>
  send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }).then((m) => {
    if (m.result?.exceptionDetails)
      console.log("EVAL-EXC:", m.result.exceptionDetails.exception?.description || m.result.exceptionDetails.text);
    return m.result?.result?.value;
  });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let fail = 0;
const check = (label, cond, extra = "") => {
  console.log(`${cond ? "ok  " : "FAIL"} ${label}${extra ? ` — ${extra}` : ""}`);
  if (!cond) fail++;
};

// open the vault
const opened = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll("button")).find(b => /open.*vault/i.test(b.textContent));
  if (!b) return "no button";
  b.click();
  return "clicked";
})()`);
if (opened !== "clicked") { console.log("FAIL: vault button not found"); process.exit(1); }
await sleep(1500);

// ---- 1. folders render, .md hidden, counts ----
const t1 = await evalJs(`(() => {
  const dirs = Array.from(document.querySelectorAll("summary.tree-dir"));
  const dirNames = dirs.map(d => d.querySelector(".dir-name")?.textContent);
  const counts = dirs.map(d => d.querySelector(".dir-count")?.textContent);
  const items = Array.from(document.querySelectorAll("button.tree-item"));
  const idea = items.find(b => b.title === "work/idea.md");
  const rootNote = items.find(b => b.title === "root.md");
  const mdVisible = items.some(b => /\\.md$/.test(b.textContent.trim()));
  return {
    dirNames,
    counts,
    hasIdea: !!idea,
    ideaText: idea?.textContent.trim(),
    hasRoot: !!rootNote,
    rootText: rootNote?.textContent.trim(),
    mdVisible,
  };
})()`);
check("folders listed", JSON.stringify(t1.dirNames) === JSON.stringify(["empty-folder", "home", "work"]), JSON.stringify(t1.dirNames));
check("folder counts (empty/home/work = 0/1/2)", JSON.stringify(t1.counts) === JSON.stringify(["0", "1", "2"]), JSON.stringify(t1.counts));
check("nested note rendered (work/idea.md)", t1.hasIdea);
check(".md hidden from note label", t1.ideaText === "idea" && t1.rootText === "root", `idea='${t1.ideaText}' root='${t1.rootText}'`);
check("no '.md' visible anywhere", !t1.mdVisible);

// ---- 2. HTML5 drag&drop: work/idea.md -> home ----
const dnd = await evalJs(`(() => {
  const src = document.querySelector('button.tree-item[title="work/idea.md"]');
  const dst = document.querySelector('li[data-drop-dir="home"]');
  if (!src || !dst) return "elements missing: src=" + !!src + " dst=" + !!dst;
  const dt = new DataTransfer();
  src.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
  return "dispatched";
})()`);
check("dnd events dispatched", dnd === "dispatched", dnd);
await sleep(600);
const dndArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "rename_note");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("drop -> rename_note to home/idea.md", dndArgs && dndArgs.newPath === "home/idea.md", JSON.stringify(dndArgs));

// drag a note OUT of its folder to the vault root
const dndRoot = await evalJs(`(() => {
  const src = document.querySelector('button.tree-item[title="work/meeting.md"]');
  const dst = document.querySelector('ul.tree[data-drop-dir=""]');
  if (!src || !dst) return "elements missing: src=" + !!src + " dst=" + !!dst;
  const dt = new DataTransfer();
  src.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
  return "dispatched";
})()`);
await sleep(600);
const dndRootArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "rename_note");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("drop on root -> rename_note to meeting.md", dndRootArgs && dndRootArgs.newPath === "meeting.md", JSON.stringify(dndRootArgs));

// ---- 3. right-click context menu on a folder ----
await evalJs(`document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }))`); // close any menu
const ctx = await evalJs(`(() => {
  const s = Array.from(document.querySelectorAll("summary.tree-dir")).find(s => s.querySelector(".dir-name")?.textContent === "home");
  if (!s) return "no folder";
  const r = s.getBoundingClientRect();
  s.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 10, clientY: r.y + 5 }));
  return "dispatched";
})()`);
await sleep(200);
const ctxMenu = await evalJs(`(() => {
  const m = document.querySelector(".ctx-menu");
  if (!m) return null;
  return Array.from(m.querySelectorAll(".ctx-item")).map(b => b.textContent.trim());
})()`);
check("folder context menu opens", ctx === "dispatched" && Array.isArray(ctxMenu), JSON.stringify(ctxMenu));
check("folder menu has actions", Array.isArray(ctxMenu) && ctxMenu.includes("New folder inside") && ctxMenu.includes("Rename folder") && ctxMenu.includes("Delete folder"), JSON.stringify(ctxMenu));

// ---- 4. right-click on a note: Move to… / Delete ----
await evalJs(`document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }))`);
const nctx = await evalJs(`(() => {
  const b = document.querySelector('button.tree-item[title="root.md"]');
  if (!b) return "no note";
  const r = b.getBoundingClientRect();
  b.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 10, clientY: r.y + 5 }));
  return "dispatched";
})()`);
await sleep(200);
const nctxMenu = await evalJs(`Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).map(b => b.textContent.trim())`);
check("note context menu opens", nctx === "dispatched" && Array.isArray(nctxMenu), JSON.stringify(nctxMenu));
check("note menu has Move to… / New folder here / Delete", Array.isArray(nctxMenu) && nctxMenu.includes("Move to…") && nctxMenu.includes("New folder here") && nctxMenu.includes("Delete note"), JSON.stringify(nctxMenu));

// "Move to…" flips into the destination picker
await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).find(x => x.textContent.trim() === "Move to…");
  b && b.click();
  return true;
})()`);
await sleep(200);
const mvMenu = await evalJs(`Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).map(b => b.textContent.trim())`);
check("move-to picker shows root + folders", Array.isArray(mvMenu) && mvMenu.includes("Vault root") && mvMenu.includes("home") && mvMenu.includes("work") && mvMenu.includes("empty-folder"), JSON.stringify(mvMenu));

// pick "home" -> rename_note(root.md -> home/root.md)
await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).find(x => x.textContent.trim() === "home");
  b && b.click();
  return true;
})()`);
await sleep(600);
const mvArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "rename_note");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("move-to applied (root.md -> home/root.md)", mvArgs && mvArgs.newPath === "home/root.md", JSON.stringify(mvArgs));

// ---- 5. touch long-press on a note lifts it; release without move -> menu ----
await evalJs(`document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }))`);
const lp = await evalJs(`(() => {
  const b = document.querySelector('button.tree-item[title="home/recipe.md"]');
  if (!b) return "no note";
  const r = b.getBoundingClientRect();
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  const t = new Touch({ identifier: 1, target: b, clientX: x, clientY: y, pageX: x, pageY: y });
  b.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t], targetTouches: [t], changedTouches: [t] }));
  window.__lpPos = { x, y };
  return "started";
})()`);
check("long-press touchstart dispatched", lp === "started", lp);
await sleep(700); // > LONG_PRESS_MS(450)
const lifted = await evalJs(`!!document.querySelector(".drag-ghost")`);
check("note lifted (ghost visible)", lifted);
const lpEnd = await evalJs(`(() => {
  const b = document.querySelector('button.tree-item[title="home/recipe.md"]');
  const { x, y } = window.__lpPos;
  const t = new Touch({ identifier: 1, target: b, clientX: x, clientY: y, pageX: x, pageY: y });
  b.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t] }));
  return true;
})()`);
void lpEnd;
await sleep(300);
const lpMenu = await evalJs(`Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).map(b => b.textContent.trim())`);
check("release without move -> note menu", Array.isArray(lpMenu) && lpMenu.includes("Move to…") && lpMenu.includes("New folder here") && lpMenu.includes("Delete note"), JSON.stringify(lpMenu));

// ---- 5b. note menu "New folder here" -> create_folder in the note's parent dir ----
const nfHere = await evalJs(`(() => {
  const b = Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).find(x => x.textContent.trim() === "New folder here");
  if (!b) return "no item";
  window.prompt = () => "nf-test";
  b.click();
  return "clicked";
})()`);
await sleep(400);
const nfArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "create_folder");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("note menu 'New folder here' -> create_folder home/nf-test", nfHere === "clicked" && nfArgs && nfArgs.path === "home/nf-test", JSON.stringify(nfArgs));

// ---- 5c. right-click anywhere else in the panel (search pad) -> root menu;
//      right-click on an input is not intercepted (native copy/paste menu) ----
const padClick = await evalJs(`(() => {
  const pad = document.querySelector(".sidebar .pad");
  if (!pad) return "no pad";
  const r = pad.getBoundingClientRect();
  pad.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 3, clientY: r.y + 3 }));
  return "dispatched";
})()`);
await sleep(200);
const rootMenu2 = await evalJs(`Array.from(document.querySelectorAll(".ctx-menu .ctx-item")).map(b => b.textContent.trim())`);
check("panel background right-click -> root menu (New folder)", padClick === "dispatched" && Array.isArray(rootMenu2) && rootMenu2.includes("New folder"), JSON.stringify(rootMenu2));

const inputClick = await evalJs(`(() => {
  document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
  const inp = document.querySelector(".sidebar input");
  if (!inp) return "no input";
  const r = inp.getBoundingClientRect();
  const ev = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 5, clientY: r.y + 5 });
  inp.dispatchEvent(ev);
  return ev.defaultPrevented ? "prevented" : "native";
})()`);
await sleep(200);
const menuAfterInput = await evalJs(`!!document.querySelector(".ctx-menu")`);
check("input right-click not intercepted", inputClick === "native" && !menuAfterInput, `${inputClick} menu=${menuAfterInput}`);

// ---- 5d. folder drag&drop: empty-folder -> home; onto itself is a no-op;
//      a note dropped on its own folder goes to the vault root ----
const fdnd = await evalJs(`(() => {
  const src = Array.from(document.querySelectorAll("summary.tree-dir")).find(s => s.querySelector(".dir-name")?.textContent === "empty-folder");
  const dst = document.querySelector('li[data-drop-dir="home"]');
  if (!src || !dst) return "elements missing: src=" + !!src + " dst=" + !!dst;
  const dt = new DataTransfer();
  src.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
  return "dispatched";
})()`);
await sleep(600);
const fdndArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "rename_folder");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("folder dnd -> rename_folder empty-folder -> home/empty-folder", fdnd === "dispatched" && fdndArgs && fdndArgs.oldPath === "empty-folder" && fdndArgs.newPath === "home/empty-folder", JSON.stringify(fdndArgs));

const selfBefore = await evalJs(`window.__invokeArgs.filter(a => a.cmd === "rename_folder").length`);
await evalJs(`(() => {
  const src = Array.from(document.querySelectorAll("summary.tree-dir")).find(s => s.querySelector(".dir-name")?.textContent === "home");
  const dst = document.querySelector('li[data-drop-dir="home"]');
  if (!src || !dst) return;
  const dt = new DataTransfer();
  src.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
})()`);
await sleep(400);
const selfAfter = await evalJs(`window.__invokeArgs.filter(a => a.cmd === "rename_folder").length`);
check("folder dropped on itself -> no rename_folder", selfAfter === selfBefore, `before=${selfBefore} after=${selfAfter}`);

const ownDrop = await evalJs(`(() => {
  const src = document.querySelector('button.tree-item[title="home/recipe.md"]');
  const dst = document.querySelector('li[data-drop-dir="home"]');
  if (!src || !dst) return "elements missing: src=" + !!src + " dst=" + !!dst;
  const dt = new DataTransfer();
  src.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt }));
  dst.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
  return "dispatched";
})()`);
await sleep(600);
const ownArgs = await evalJs(`(() => {
  const rs = window.__invokeArgs.filter(a => a.cmd === "rename_note");
  return rs.length ? rs[rs.length - 1].args : null;
})()`);
check("note dropped on its own folder -> vault root (recipe.md)", ownDrop === "dispatched" && ownArgs && ownArgs.newPath === "recipe.md", JSON.stringify(ownArgs));

// ---- 6. edge swipes: close (left swipe in sidebar) then open (right swipe from left edge) ----
const closed = await evalJs(`(() => {
  const target = document.querySelector(".sidebar .tree-item") || document.querySelector(".sidebar input");
  if (!target) return "no sidebar target";
  const t1 = new Touch({ identifier: 2, target, clientX: 100, clientY: 300, pageX: 100, pageY: 300 });
  target.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 2, target, clientX: 10, clientY: 300, pageX: 10, pageY: 300 });
  target.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t2] }));
  return true;
})()`);
void closed;
await sleep(300);
const sidebarAfterClose = await evalJs(`!!document.querySelector(".sidebar")`);
check("swipe left in sidebar closes it", closed === true && !sidebarAfterClose, `sidebar=${sidebarAfterClose}`);

const openedSwipe = await evalJs(`(() => {
  const target = document.elementFromPoint(5, 300) || document.body;
  const t1 = new Touch({ identifier: 3, target, clientX: 5, clientY: 300, pageX: 5, pageY: 300 });
  target.dispatchEvent(new TouchEvent("touchstart", { bubbles: true, cancelable: true, touches: [t1], targetTouches: [t1], changedTouches: [t1] }));
  const t2 = new Touch({ identifier: 3, target, clientX: 150, clientY: 300, pageX: 150, pageY: 300 });
  target.dispatchEvent(new TouchEvent("touchend", { bubbles: true, cancelable: true, touches: [], targetTouches: [], changedTouches: [t2] }));
  return true;
})()`);
void openedSwipe;
await sleep(300);
const sidebarAfterOpen = await evalJs(`!!document.querySelector(".sidebar")`);
check("swipe right from left edge opens it", openedSwipe === true && sidebarAfterOpen, `sidebar=${sidebarAfterOpen}`);

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
console.log(fail ? `\n${fail} FAILURES` : "all tree checks pass");
process.exit(fail ? 1 : 0);
