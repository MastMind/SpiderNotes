import { useEffect, useMemo, useRef, useState } from "react";
import type { NoteSummary } from "./types";
import { WarnIcon } from "./icons";

interface TreeNode {
  name: string;
  /** Folder's relative path; null for note nodes. */
  folderPath: string | null;
  note: NoteSummary | null;
  children: Map<string, TreeNode>;
}

type Menu =
  | { x: number; y: number; kind: "note"; note: NoteSummary }
  | { x: number; y: number; kind: "folder"; path: string }
  | { x: number; y: number; kind: "root" }
  | { x: number; y: number; kind: "moveTo"; note: NoteSummary };

const LONG_PRESS_MS = 450;
const MOVE_SLOP_PX = 8;

function stripMd(name: string): string {
  return name.replace(/\.md$/i, "");
}

// Compact "when" for list rows and the sync footer: today -> "12:34",
// this year -> "26.11", older -> "26.11.24" (ru-RU, like the rest of the UI).
export function formatWhen(ms: number): string {
  const d = new Date(ms);
  const now = new Date();
  if (d.toDateString() === now.toDateString())
    return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
  const opts: Intl.DateTimeFormatOptions =
    d.getFullYear() === now.getFullYear()
      ? { day: "2-digit", month: "2-digit" }
      : { day: "2-digit", month: "2-digit", year: "2-digit" };
  return d.toLocaleDateString("ru-RU", opts);
}

// "device · time" — the device is the HLC short id, resolved to a friendly
// name via the map from App (own device + known peers); unknown ids show
// as a short fragment.
function rowMeta(n: NoteSummary, deviceNames: Map<string, string>): string {
  const dev = deviceNames.get(n.hlc.device) ?? n.hlc.device.slice(0, 6);
  return `${dev} · ${formatWhen(n.hlc.ms)}`;
}

/// Theme-aware folder glyph (SVG + currentColor — follows every color
/// scheme; the emoji did not). The open/closed state is switched by CSS
/// on `details[open]`, so both variants are rendered.
function FolderIcon() {
  return (
    <span className="dir-icon" aria-hidden>
      <svg className="di-closed" viewBox="0 0 16 16" width="13" height="13">
        <path
          d="M1.5 12.5v-8c0-.55.45-1 1-1h3.1c.3 0 .58.15.76.4l.9 1.15H13c.55 0 1 .45 1 1v6.45c0 .55-.45 1-1 1H2.5c-.55 0-1-.45-1-1z"
          fill="currentColor"
          opacity=".5"
        />
        <rect x="1.5" y="7.6" width="13" height="5.9" rx="1" fill="currentColor" />
      </svg>
      <svg className="di-open" viewBox="0 0 16 16" width="13" height="13">
        <path
          d="M1.5 12.5v-8c0-.55.45-1 1-1h3.1c.3 0 .58.15.76.4l.9 1.15H13c.55 0 1 .45 1 1v6.45c0 .55-.45 1-1 1H2.5c-.55 0-1-.45-1-1z"
          fill="currentColor"
          opacity=".5"
        />
        <path d="M1.8 7.5h13l-1.5 5.5H.3z" fill="currentColor" />
      </svg>
    </span>
  );
}

function buildTree(notes: NoteSummary[], folders: string[]): TreeNode[] {
  const root = new Map<string, TreeNode>();
  const ensureFolder = (path: string): Map<string, TreeNode> => {
    const parts = path.split("/");
    let level = root;
    let acc = "";
    for (const part of parts) {
      acc = acc ? `${acc}/${part}` : part;
      let node = level.get(part);
      if (!node) {
        node = { name: part, folderPath: acc, note: null, children: new Map() };
        level.set(part, node);
      }
      level = node.children;
    }
    return level;
  };
  for (const f of folders) ensureFolder(f);
  for (const n of notes) {
    const parts = n.path.split("/");
    let level = root;
    for (let i = 0; i < parts.length - 1; i++) {
      const part = parts[i];
      let node = level.get(part);
      if (!node || node.note) {
        node = {
          name: part,
          folderPath: parts.slice(0, i + 1).join("/"),
          note: null,
          children: new Map(),
        };
        level.set(part, node);
      }
      level = node.children;
    }
    const last = parts[parts.length - 1];
    level.set(last, {
      name: stripMd(last),
      folderPath: null,
      note: n,
      children: new Map(),
    });
  }
  return collect(root);
}

function collect(map: Map<string, TreeNode>): TreeNode[] {
  return [...map.values()].sort(
    (a, b) =>
      Number(!!b.folderPath) - Number(!!a.folderPath) ||
      a.name.localeCompare(b.name),
  );
}

function countNotesUnder(notes: NoteSummary[], folder: string): number {
  const prefix = `${folder}/`;
  return notes.filter((n) => n.path.startsWith(prefix)).length;
}

export function NotesTree({
  notes,
  folders,
  allFolders,
  selected,
  onSelect,
  onCreateFolder,
  onRenameFolder,
  onDeleteFolder,
  onMoveNote,
  onDeleteNote,
  onMoveFolder,
  deviceNames,
  conflictCount,
  onConflict,
}: {
  notes: NoteSummary[];
  folders: string[];
  /** Full folder list for the "Move to…" picker (unfiltered by search). */
  allFolders: string[];
  /** Device short id (HLC) -> friendly name, for the row meta line. */
  deviceNames: Map<string, string>;
  /** Note uuid -> number of pending sync conflicts (warning icon). */
  conflictCount: Map<string, number>;
  /** Open the conflict panel for a note. */
  onConflict: (uuid: string) => void;
  selected: string | null;
  onSelect: (uuid: string) => void;
  onCreateFolder: (parentDir: string) => void;
  onRenameFolder: (path: string) => void;
  onDeleteFolder: (path: string) => void;
  onMoveNote: (uuid: string, destDir: string | null) => void;
  onDeleteNote: (uuid: string) => void;
  /** Drag a folder into another one ("" = vault root). */
  onMoveFolder: (oldPath: string, destDir: string) => void;
}) {
  const tree = useMemo(() => buildTree(notes, folders), [notes, folders]);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [dropDir, setDropDir] = useState<string | null>(null);
  const [ghost, setGhost] = useState<{ label: string; x: number; y: number } | null>(null);

  const wrapRef = useRef<HTMLDivElement>(null);
  const dragNoteRef = useRef<NoteSummary | null>(null);
  const dragFolderRef = useRef<string | null>(null);
  const suppressClickRef = useRef(false);
  const lpTimer = useRef<number | null>(null);
  const lpStart = useRef<{ x: number; y: number } | null>(null);

  const clearLpTimer = () => {
    if (lpTimer.current !== null) {
      window.clearTimeout(lpTimer.current);
      lpTimer.current = null;
    }
  };

  // Right-click anywhere in the notes panel (handled on the sidebar in
  // App.tsx) opens the root menu — items stopPropagation and keep their
  // own menus.
  useEffect(() => {
    const onRoot = (e: Event) => {
      const d = (e as CustomEvent).detail as { x: number; y: number };
      setMenu({ x: d.x, y: d.y, kind: "root" });
    };
    window.addEventListener("sn-root-menu", onRoot);
    return () => window.removeEventListener("sn-root-menu", onRoot);
  }, []);

  // Close the context menu on any tap/scroll outside it.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onDown = (e: PointerEvent) => {
      const t = e.target as Element | null;
      if (t && t.closest(".ctx-menu")) return;
      close();
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  const dropDirFrom = (el: EventTarget | null): string | null => {
    const e = el as Element | null;
    const host = e?.closest?.("[data-drop-dir]");
    if (!host || !wrapRef.current?.contains(host)) return null;
    return host.getAttribute("data-drop-dir");
  };

  // ---- desktop drag & drop ----
  const onDragStart = (e: React.DragEvent, note: NoteSummary) => {
    dragNoteRef.current = note;
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", note.path);
  };
  const onFolderDragStart = (e: React.DragEvent, path: string) => {
    dragFolderRef.current = path;
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", path);
  };
  const onDragOver = (e: React.DragEvent) => {
    const n = dragNoteRef.current;
    const f = dragFolderRef.current;
    if (!n && !f) return;
    const dir = dropDirFrom(e.target);
    if (dir === null) return;
    // a folder cannot be dropped onto itself or into its own subtree
    if (f && (dir === f || dir.startsWith(`${f}/`))) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    setDropDir(dir);
  };
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    const dir = dropDirFrom(e.target);
    const n = dragNoteRef.current;
    const f = dragFolderRef.current;
    setDropDir(null);
    dragNoteRef.current = null;
    dragFolderRef.current = null;
    if (f && dir !== null && dir !== f && !dir.startsWith(`${f}/`)) {
      onMoveFolder(f, dir);
      return;
    }
    if (n && dir !== null) {
      // dropping a note onto its own folder means "out of it": vault root
      const curDir =
        n.path.lastIndexOf("/") >= 0
          ? n.path.slice(0, n.path.lastIndexOf("/"))
          : "";
      onMoveNote(n.uuid, dir === curDir && curDir !== "" ? "" : dir);
    }
  };
  const onDragLeave = (e: React.DragEvent) => {
    if (!wrapRef.current?.contains(e.relatedTarget as Node | null)) {
      setDropDir(null);
    }
  };

  // ---- touch: long-press lifts a note (drag it) or opens a menu ----
  const startLongPress = (
    e: React.TouchEvent,
    kind: "note" | "folder" | "root",
    payload: { note?: NoteSummary; folder?: string },
  ) => {
    clearLpTimer();
    // a fresh touch invalidates a leftover "suppress the next click" flag
    // (it is only meaningful for the click right after a drag release)
    suppressClickRef.current = false;
    const t = e.touches[0];
    if (!t) return;
    lpStart.current = { x: t.clientX, y: t.clientY };
    lpTimer.current = window.setTimeout(() => {
      lpTimer.current = null;
      const s = lpStart.current;
      if (!s) return;
      if (kind === "note" && payload.note) {
        // lift: the note becomes a floating ghost following the finger
        setGhost({
          label: payload.note.path.split("/").pop() ?? payload.note.path,
          x: s.x,
          y: s.y,
        });
        startTouchDrag(payload.note);
      } else if (kind === "folder" && payload.folder) {
        setMenu({ x: s.x, y: s.y, kind: "folder", path: payload.folder });
      } else {
        setMenu({ x: s.x, y: s.y, kind: "root" });
      }
    }, LONG_PRESS_MS);
  };

  const startTouchDrag = (note: NoteSummary) => {
    dragNoteRef.current = note;
    let moved = 0;
    const onMove = (e: TouchEvent) => {
      const t = e.touches[0];
      if (!t) return;
      e.preventDefault();
      const s = lpStart.current;
      if (s) {
        moved = Math.max(
          moved,
          Math.hypot(t.clientX - s.x, t.clientY - s.y),
        );
      }
      setGhost((g) => (g ? { ...g, x: t.clientX, y: t.clientY } : g));
      setDropDir(dropDirFrom(document.elementFromPoint(t.clientX, t.clientY)));
    };
    const onEnd = (e: TouchEvent) => {
      cleanup();
      const t = e.changedTouches[0];
      const dir =
        t != null
          ? dropDirFrom(document.elementFromPoint(t.clientX, t.clientY))
          : null;
      setGhost(null);
      setDropDir(null);
      suppressClickRef.current = true;
      const n = dragNoteRef.current;
      if (n && t != null) {
        // the folder the note already lives in
        const curDir =
          n.path.lastIndexOf("/") >= 0
            ? n.path.slice(0, n.path.lastIndexOf("/"))
            : "";
        if (dir !== null && dir !== curDir) {
          onMoveNote(n.uuid, dir);
        } else if (dir === curDir && curDir !== "" && moved >= MOVE_SLOP_PX) {
          // dropped back onto its own folder: "out of it" -> vault root
          onMoveNote(n.uuid, "");
        } else if (moved < MOVE_SLOP_PX && (dir === null || dir === curDir)) {
          // lifted, but the finger barely moved (still over its own row):
          // treat as a long-press menu
          setMenu({ x: t.clientX, y: t.clientY, kind: "note", note: n });
        }
      }
      dragNoteRef.current = null;
    };
    const cleanup = () => {
      document.removeEventListener("touchmove", onMove);
      document.removeEventListener("touchend", onEnd);
      document.removeEventListener("touchcancel", onEnd);
    };
    document.addEventListener("touchmove", onMove, { passive: false });
    document.addEventListener("touchend", onEnd);
    document.addEventListener("touchcancel", onEnd);
  };

  // a finger moving past the slop means scrolling, not pressing
  const onTouchMoveCancel = (e: React.TouchEvent) => {
    if (lpTimer.current === null) return;
    const t = e.touches[0];
    const s = lpStart.current;
    if (t && s && Math.hypot(t.clientX - s.x, t.clientY - s.y) > MOVE_SLOP_PX) {
      clearLpTimer();
    }
  };
  const onTouchEndCancel = () => clearLpTimer();

  const renderNode = (node: TreeNode, parentDir: string) => {
    if (node.note) {
      const n = node.note;
      return (
        <li key={n.uuid} data-drop-dir={parentDir}>
          <button
            className={selected === n.uuid ? "tree-item sel" : "tree-item"}
            title={n.path}
            draggable
            onDragStart={(e) => onDragStart(e, n)}
            onClick={() => {
              if (suppressClickRef.current) {
                suppressClickRef.current = false;
                return;
              }
              onSelect(n.uuid);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setMenu({ x: e.clientX, y: e.clientY, kind: "note", note: n });
            }}
            onTouchStart={(e) => startLongPress(e, "note", { note: n })}
            onTouchMove={onTouchMoveCancel}
            onTouchEnd={onTouchEndCancel}
          >
            <span className="row-top">
              <span className="row-title">{n.title || node.name}</span>
              {conflictCount.get(n.uuid) ? (
                // a span, not a button: the row itself is a button (nested
                // buttons are invalid) — stopPropagation keeps the row click
                <span
                  className="row-conflict"
                  title="Sync conflict: open to choose a version"
                  onClick={(e) => {
                    e.stopPropagation();
                    onConflict(n.uuid);
                  }}
                >
                  <WarnIcon />
                </span>
              ) : null}
              <span className="row-meta">{rowMeta(n, deviceNames)}</span>
            </span>
            {n.preview ? <span className="row-preview">{n.preview}</span> : null}
          </button>
        </li>
      );
    }
    const path = node.folderPath!;
    return (
      <li key={path} className="tree-li-dir" data-drop-dir={path}>
        <details open>
          <summary
            className={dropDir === path ? "tree-dir drop-target" : "tree-dir"}
            draggable
            onDragStart={(e) => onFolderDragStart(e, path)}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setMenu({ x: e.clientX, y: e.clientY, kind: "folder", path });
            }}
            onTouchStart={(e) => startLongPress(e, "folder", { folder: path })}
            onTouchMove={onTouchMoveCancel}
            onTouchEnd={onTouchEndCancel}
          >
            <svg
              className="dir-chev"
              viewBox="0 0 8 8"
              width="8"
              height="8"
              aria-hidden
            >
              <path d="M2.2 1l3.6 3-3.6 3z" fill="currentColor" />
            </svg>
            <FolderIcon />
            <span className="dir-name">{node.name}</span>
            <span className="muted dir-count">{countNotesUnder(notes, path)}</span>
          </summary>
          <ul className="tree">
            {collect(node.children).map((c) => renderNode(c, path))}
          </ul>
        </details>
      </li>
    );
  };

  if (!notes.length && !folders.length) {
    return (
      <div
        className="muted pad tree-empty"
        onTouchStart={(e) => startLongPress(e, "root", {})}
        onTouchMove={onTouchMoveCancel}
        onTouchEnd={onTouchEndCancel}
      >
        No notes yet
      </div>
    );
  }

  return (
    <div
      ref={wrapRef}
      className="tree-host"
      onDragOver={onDragOver}
      onDrop={onDrop}
      onDragLeave={onDragLeave}
      onTouchStart={(e) => {
        // long-press on empty space (not on an item) opens the root menu
        const t = e.target as Element | null;
        if (t && (t.closest(".tree-item") || t.closest(".tree-dir"))) return;
        startLongPress(e, "root", {});
      }}
      onTouchMove={onTouchMoveCancel}
      onTouchEnd={onTouchEndCancel}
    >
      <ul className="tree" data-drop-dir="">
        {tree.map((n) => renderNode(n, ""))}
      </ul>
      {dropDir !== null && !menu && (
        <div className="drop-hint muted">
          {(() => {
            // hovering a note's own folder means "out of it" -> root
            const dn = dragNoteRef.current;
            const curDir = dn
              ? dn.path.lastIndexOf("/") >= 0
                ? dn.path.slice(0, dn.path.lastIndexOf("/"))
                : ""
              : null;
            if (dropDir === "" || (curDir !== null && dropDir === curDir))
              return "→ vault root";
            return `→ ${dropDir}`;
          })()}
        </div>
      )}
      {ghost && (
        <div className="drag-ghost" style={{ left: ghost.x, top: ghost.y }}>
          {ghost.label}
        </div>
      )}
      {menu && (
        <ContextMenu
          menu={menu}
          folders={allFolders}
          onMenu={setMenu}
          onNewFolder={onCreateFolder}
          onRenameFolder={onRenameFolder}
          onDeleteFolder={onDeleteFolder}
          onMoveNote={onMoveNote}
          onDeleteNote={onDeleteNote}
        />
      )}
    </div>
  );
}

interface CtxItem {
  label: string;
  danger?: boolean;
  indent?: number;
  /** Item replaces the menu instead of closing it (Move to… / Back). */
  keepOpen?: boolean;
  run: () => void;
}

function ContextMenu({
  menu,
  folders,
  onMenu,
  onNewFolder,
  onRenameFolder,
  onDeleteFolder,
  onMoveNote,
  onDeleteNote,
}: {
  menu: Menu;
  folders: string[];
  /** Replace the current menu (e.g. note -> moveTo picker) without closing. */
  onMenu: (m: Menu | null) => void;
  onNewFolder: (parentDir: string) => void;
  onRenameFolder: (path: string) => void;
  onDeleteFolder: (path: string) => void;
  onMoveNote: (uuid: string, destDir: string | null) => void;
  onDeleteNote: (uuid: string) => void;
}) {
  const x = Math.max(4, Math.min(menu.x, window.innerWidth - 200));
  const y = Math.max(4, Math.min(menu.y, window.innerHeight - 280));

  let title = "Vault";
  let items: CtxItem[] = [{ label: "New folder", run: () => onNewFolder("") }];
  if (menu.kind === "note") {
    const n = menu.note;
    // parent directory of the note ("" = vault root)
    const parentDir = n.path.includes("/")
      ? n.path.slice(0, n.path.lastIndexOf("/"))
      : "";
    title = "Note";
    items = [
      {
        label: "Move to…",
        // switch the open menu into the destination picker (no close/reopen)
        keepOpen: true,
        run: () => onMenu({ x: menu.x, y: menu.y, kind: "moveTo", note: n }),
      },
      { label: "New folder here", run: () => onNewFolder(parentDir) },
      { label: "Delete note", danger: true, run: () => onDeleteNote(n.uuid) },
    ];
  } else if (menu.kind === "moveTo") {
    const n = menu.note;
    title = "Move to";
    items = [
      { label: "Vault root", run: () => onMoveNote(n.uuid, null) },
      ...folders.map((f) => ({
        label: f,
        indent: f.split("/").length - 1,
        run: () => onMoveNote(n.uuid, f),
      })),
      {
        label: "Back",
        keepOpen: true,
        run: () => onMenu({ x: menu.x, y: menu.y, kind: "note", note: n }),
      },
    ];
  } else if (menu.kind === "folder") {
    title = "Folder";
    items = [
      { label: "New folder inside", run: () => onNewFolder(menu.path) },
      { label: "Rename folder", run: () => onRenameFolder(menu.path) },
      { label: "Delete folder", danger: true, run: () => onDeleteFolder(menu.path) },
    ];
  }

  return (
    <div className="ctx-menu" style={{ left: x, top: y }} onContextMenu={(e) => e.preventDefault()}>
      <div className="ctx-title">{title}</div>
      {items.map((it, i) => (
        <button
          key={i}
          className={it.danger ? "ctx-item danger" : "ctx-item"}
          onClick={() => {
            it.run();
            if (!it.keepOpen) onMenu(null);
          }}
        >
          {it.indent ? `${"  • ".repeat(it.indent)}${it.label}` : it.label}
        </button>
      ))}
    </div>
  );
}
