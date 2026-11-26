import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "./api";
import type { Backlink, Conflict, NoteSummary, OpenResult, PeerInfo, P2pSettings, TagCount } from "./types";
import { Editor } from "./Editor";
import { NotesTree, formatWhen } from "./NotesTree";
import { PeersPanel } from "./PeersPanel";
import { THEMES } from "./themes";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  CloseIcon,
  GearIcon,
  GlobeIcon,
  NoteIcon,
  PlusIcon,
  SearchIcon,
  SyncIcon,
  WarnIcon,
  WifiIcon,
  WifiOffIcon,
} from "./icons";

function savedVaultPath(): string {
  return localStorage.getItem("sn.vaultPath") || "";
}

function ts(): string {
  return new Date().toLocaleTimeString("ru-RU");
}

export default function App() {
  const [open, setOpen] = useState<OpenResult | null>(null);
  const [pathInput, setPathInput] = useState(savedVaultPath());
  const [notes, setNotes] = useState<NoteSummary[]>([]);
  const [folders, setFolders] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [doc, setDoc] = useState("");
  const [dirty, setDirty] = useState(false);
  const [query, setQuery] = useState("");
  const [peers, setPeers] = useState<PeerInfo[]>([]);
  // sync footer data (refreshed with the note list, i.e. after every sync)
  const [p2p, setP2p] = useState<P2pSettings | null>(null);
  const [ssOn, setSsOn] = useState(false);
  // pending sync conflicts (three-way merge leftovers); the note rows show
  // a warning icon, the panel below offers both versions for a decision
  const [conflicts, setConflicts] = useState<Conflict[]>([]);
  const [conflictView, setConflictView] = useState<Conflict | null>(null);
  const [conflictParts, setConflictParts] = useState<{
    ours: string;
    theirs: string;
  } | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [newName, setNewName] = useState("");
  const [searchResults, setSearchResults] = useState<NoteSummary[] | null>(null);
  const [tags, setTags] = useState<TagCount[]>([]);
  const [activeTag, setActiveTag] = useState<string | null>(null);
  const [tagNotes, setTagNotes] = useState<NoteSummary[] | null>(null);
  const [backlinks, setBacklinks] = useState<Backlink[]>([]);
  const [showNet, setShowNet] = useState(
    () => localStorage.getItem("sn.showNet") === "1",
  );
  // Notes list panel; on narrow screens it starts hidden so the editor gets
  // the whole width (the choice is persisted after the first toggle).
  const [showSidebar, setShowSidebar] = useState(() => {
    const v = localStorage.getItem("sn.showSidebar");
    if (v === null) return window.innerWidth > 700;
    return v === "1";
  });
  // Notes-list panel width in px (desktop, drag the divider; 180..420).
  const [sidebarW, setSidebarW] = useState(() => {
    const n = parseInt(localStorage.getItem("sn.sidebarW") || "", 10);
    return Number.isFinite(n) && n >= 180 && n <= 420 ? n : 250;
  });
  const sidebarWRef = useRef(sidebarW);
  sidebarWRef.current = sidebarW;

  const startResize = useCallback((e: React.PointerEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarWRef.current;
    const move = (ev: PointerEvent) => {
      const w = Math.max(180, Math.min(420, startW + (ev.clientX - startX)));
      setSidebarW(w);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      localStorage.setItem("sn.sidebarW", String(sidebarWRef.current));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }, []);

  const resetSidebarW = useCallback(() => {
    setSidebarW(250);
    localStorage.setItem("sn.sidebarW", "250");
  }, []);
  // live preview (markdown rendered inline in the editor); off = raw source
  const [live, setLive] = useState(
    () => localStorage.getItem("sn.live") !== "0",
  );
  // UI theme id (see THEMES in themes.ts); "dark" is the default.
  const [theme, setTheme] = useState<string>(() => {
    const t = localStorage.getItem("sn.theme");
    return THEMES.some((x) => x.id === t) ? (t as string) : "dark";
  });
  // Note text size in px (design setting; 12..18, default 14). Scales only
  // the editor content (--note-fs); the UI chrome keeps its base size.
  const [fontSize, setFontSize] = useState<number>(() => {
    const n = parseInt(localStorage.getItem("sn.fontSize") || "", 10);
    return Number.isFinite(n) && n >= 12 && n <= 18 ? n : 14;
  });
  // Note text font family (design setting): "mono" (default) or "sans"
  // (the bundled Montserrat).
  const [noteFont, setNoteFont] = useState<string>(() =>
    localStorage.getItem("sn.noteFont") === "sans" ? "sans" : "mono"
  );
  // Editor line-number gutter (design setting; on by default).
  const [lineNumbers, setLineNumbers] = useState<boolean>(
    () => localStorage.getItem("sn.lineNumbers") !== "0",
  );
  // Autosave delay in seconds (frontend-only setting; default 2s).
  const [autosaveSecs, setAutosaveSecs] = useState(() => {
    const n = parseInt(localStorage.getItem("sn.autosaveSecs") || "", 10);
    return Number.isFinite(n) && n >= 1 ? n : 2;
  });

  const saveTimer = useRef<number | null>(null);
  const docRef = useRef(doc);
  docRef.current = doc;
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  const notesRef = useRef(notes);
  notesRef.current = notes;
  const foldersRef = useRef(folders);
  foldersRef.current = folders;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const autosaveMsRef = useRef(autosaveSecs * 1000);
  autosaveMsRef.current = autosaveSecs * 1000;

  // device short id (as stored in the HLC) -> friendly name, for the note
  // rows: the last editor is usually this device or a known peer.
  const deviceNames = useMemo(() => {
    const m = new Map<string, string>();
    const add = (id: string, name: string) => {
      if (id && name) m.set(id.slice(0, 16), name);
    };
    if (open) add(open.device_id, open.device_name);
    for (const p of peers) add(p.device_id, p.name);
    return m;
  }, [open, peers]);

  // note uuid -> number of pending conflicts (the row warning icon)
  const conflictCount = useMemo(() => {
    const m = new Map<string, number>();
    for (const c of conflicts)
      m.set(c.note_uuid, (m.get(c.note_uuid) ?? 0) + 1);
    return m;
  }, [conflicts]);

  const addLog = useCallback((line: string) => {
    setLog((l) => [...l.slice(-99), `${ts()}  ${line}`]);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [n, t, f, p, s, cf] = await Promise.all([
        api.listNotes(),
        api.allTags(),
        api.listFolders(),
        api.p2pSettings(),
        api.serverSyncStatus(),
        api.listConflicts(),
      ]);
      setNotes(n);
      setTags(t);
      setFolders(f);
      setP2p(p);
      setSsOn(s.enabled);
      // older mocks / a failing call yield null: no conflicts then
      setConflicts(cf ?? []);
    } catch (e) {
      addLog(`list read error: ${e}`);
    }
  }, [addLog]);

  // Open the conflict panel for a note (the row's warning icon).
  const openConflict = useCallback(
    (uuid: string) => {
      const c = conflicts.find((x) => x.note_uuid === uuid);
      if (!c) return;
      setConflictView(c);
      setConflictParts(null);
      void (async () => {
        const [ours, theirs] = await Promise.all([
          api
            .readNote(c.note_uuid)
            .then((n) => n.content)
            .catch(() => "(this version was deleted)"),
          api.conflictContent(c.id).catch(() => "(version unavailable)"),
        ]);
        setConflictParts({ ours, theirs });
      })();
    },
    [conflicts],
  );

  const closeConflict = useCallback(() => {
    setConflictView(null);
    setConflictParts(null);
  }, []);

  const doResolveConflict = useCallback(
    async (keepTheirs: boolean) => {
      const c = conflictView;
      if (!c) return;
      try {
        await api.resolveConflict(c.id, keepTheirs);
        addLog(
          `conflict ${c.note_path}: kept ${keepTheirs ? `"${c.peer_name}"` : "this device's"} version`,
        );
        closeConflict();
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [conflictView, addLog, closeConflict, refresh],
  );

  // Re-read the currently open note after a sync: the remote side may have
  // changed (or deleted) it, and the editor only follows the `doc` prop.
  const refreshOpenNote = useCallback(async () => {
    const uuid = selectedRef.current;
    if (!uuid) return;
    // Local edits still in flight: they save shortly and the follow-up
    // notes-changed (from the push-sync) reloads the merged content.
    if (dirtyRef.current) return;
    try {
      const note = await api.readNote(uuid);
      // Re-check AFTER the await: a keystroke landing while the read was in
      // flight makes the user dirty again — applying the (stale) disk content
      // would clobber the fresh input, and the pending autosave would then
      // persist the clobber. Check + setDoc below are one synchronous block,
      // so no keystroke can interleave.
      if (dirtyRef.current) return;
      if (note.content !== docRef.current) {
        setDoc(note.content);
        setDirty(false);
      }
    } catch {
      // The note was deleted remotely: verify against a fresh list and
      // close the editor if it is gone.
      if (dirtyRef.current) return; // fresh input in flight: autosave recreates it
      const list = await api.listNotes().catch(() => null);
      if (list && !list.some((x) => x.uuid === uuid)) {
        setSelected(null);
        setDoc("");
      }
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await api.vaultStatus();
        if (cancelled || !res) return;
        setOpen(res);
        setNotes(res.notes);
        setPeers(res.peers);
        setFolders(res.folders);
        // populate the sync footer (p2p settings + relay state)
        if (!cancelled) void refresh();
      } catch {
        // not in a Tauri context (plain browser) — stay on the welcome screen
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const un1 = listen<PeerInfo[]>("peers-updated", (e) => setPeers(e.payload));
    const un2 = listen("notes-changed", () => {
      void refresh();
      void refreshOpenNote();
    });
    return () => {
      void un1.then((f) => f());
      void un2.then((f) => f());
    };
  }, [refresh, refreshOpenNote]);

  // Apply the persisted advanced settings to the backend early, so the first
  // P2P start (on vault open) already uses the saved port / interval.
  useEffect(() => {
    const applyStored = async () => {
      try {
        const a = parseInt(localStorage.getItem("sn.autosyncSecs") || "", 10);
        if (a > 0) await api.setAutosyncSecs(a);
        const p = parseInt(localStorage.getItem("sn.p2pPort") || "", 10);
        if (p > 0) await api.setP2pPortSetting(p);
        // background sync (Android foreground service) is on unless disabled
        await api.setBgSync(localStorage.getItem("sn.bgSync") !== "0");
        // relay server: re-enable the stored room (backend state is
        // in-memory, so nothing persists across app restarts)
        const ss = JSON.parse(
          localStorage.getItem("sn.serverSync") || "null"
        ) as { enabled: boolean; host: string; port: number; room: string; secret: string } | null;
        if (ss && ss.enabled && ss.host && ss.room && ss.secret) {
          await api.setServerSync(true, ss.host, ss.port, ss.room, ss.secret);
        }
      } catch {
        // not in a Tauri context; ignore
      }
    };
    void applyStored();
  }, []);

 useEffect(() => {
    localStorage.setItem("sn.showNet", showNet ? "1" : "0");
    localStorage.setItem("sn.live", live ? "1" : "0");
  }, [showNet, live]);

  useEffect(() => {
    localStorage.setItem("sn.showSidebar", showSidebar ? "1" : "0");
  }, [showSidebar]);

  // Returning to the foreground (Android): the multicast socket dies while
  // backgrounded, so rebuild the mDNS daemon right away instead of waiting
  // for the 60s tick — and sync immediately with the last known peers (the
  // in-memory cache survives a background freeze; mDNS re-discovery may lag).
  useEffect(() => {
    let last = 0;
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      const now = Date.now();
      if (now - last < 5000) return; // ignore rapid hide/show churn
      last = now;
      void api.mdnsRefresh();
      void api.syncAll().catch(() => {}); // no vault / no peers: ignore
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, []);

  useEffect(() => {
    localStorage.setItem("sn.theme", theme);
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // Note text size: only the editor content scales from --note-fs
  // (styles.css); the UI chrome keeps its base --ui-fs.
  useEffect(() => {
    localStorage.setItem("sn.fontSize", String(fontSize));
    document.documentElement.style.setProperty("--note-fs", `${fontSize}px`);
  }, [fontSize]);

  useEffect(() => {
    localStorage.setItem("sn.noteFont", noteFont);
    document.documentElement.dataset.noteFont = noteFont;
  }, [noteFont]);

  useEffect(() => {
    localStorage.setItem("sn.lineNumbers", lineNumbers ? "1" : "0");
  }, [lineNumbers]);

  // Edge swipes (touch): from the screen's left edge, swipe RIGHT to open
  // the notes list; from within the open list, swipe LEFT to close it.
  // A long-press note drag renders a .drag-ghost — while it exists the
  // close-swipe is ignored so a drag never collapses the panel.
  const showSidebarRef = useRef(showSidebar);
  showSidebarRef.current = showSidebar;
  useEffect(() => {
    let start: {
      x: number;
      y: number;
      inSidebar: boolean;
      inText: boolean;
    } | null = null;
    const onStart = (e: TouchEvent) => {
      const t = e.touches[0];
      if (!t) return;
      const el = e.target as Element | null;
      start = {
        x: t.clientX,
        y: t.clientY,
        inSidebar: !!el?.closest?.(".sidebar"),
        // a horizontal drag starting here moves the caret / scrolls the
        // overlay — never treat it as a navigation swipe. The EDITOR is
        // deliberately NOT excluded: with wrapped lines and no horizontal
        // scroll a horizontal swipe there does nothing, and excluding it
        // would make the list un-openable while a note is open (the editor
        // then covers the whole screen).
        inText: !!el?.closest?.("input, textarea, .panel"),
      };
    };
    const onEnd = (e: TouchEvent) => {
      const s = start;
      start = null;
      const t = e.changedTouches[0];
      if (!s || !t) return;
      const dx = t.clientX - s.x;
      const dy = t.clientY - s.y;
      if (Math.abs(dx) < 60 || Math.abs(dx) <= 1.5 * Math.abs(dy)) return;
      const isMobile = window.matchMedia("(max-width: 700px)").matches;
      if (
        dx < 0 &&
        s.inSidebar &&
        showSidebarRef.current &&
        !document.querySelector(".drag-ghost")
      ) {
        setShowSidebar(false);
      } else if (dx > 0 && !showSidebarRef.current) {
        // open: desktop — from the left 24px edge (the rail is right
        // there); mobile — from anywhere on the screen (the rail is
        // hidden), except inside text inputs, the editor or the settings
        // panel, where a horizontal drag has its own meaning
        if (isMobile ? !s.inText : s.x <= 24) {
          setShowSidebar(true);
        }
      }
    };
    window.addEventListener("touchstart", onStart, { passive: true });
    window.addEventListener("touchend", onEnd, { passive: true });
    return () => {
      window.removeEventListener("touchstart", onStart);
      window.removeEventListener("touchend", onEnd);
    };
  }, []);

  // First launch: fill the vault path with a platform-appropriate default
  // (desktop: ~/Documents/SpiderNotes; mobile: the app's internal storage).
  useEffect(() => {
    if (!savedVaultPath()) {
      api
        .defaultVaultPath()
        .then(setPathInput)
        .catch(() => setPathInput("~/Documents/SpiderNotes"));
    }
  }, []);

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setSearchResults(null);
      return;
    }
    let cancelled = false;
    const t = window.setTimeout(async () => {
      try {
        const r = await api.searchNotes(q);
        if (!cancelled) setSearchResults(r);
      } catch {
        if (!cancelled) setSearchResults([]);
      }
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(t);
    };
  }, [query]);

  useEffect(() => {
    if (!open) {
      setTagNotes(null);
      return;
    }
    if (!activeTag) {
      setTagNotes(null);
      return;
    }
    let cancelled = false;
    api
      .notesWithTag(activeTag)
      .then((r) => {
        if (!cancelled) setTagNotes(r);
      })
      .catch(() => {
        if (!cancelled) setTagNotes([]);
      });
    return () => {
      cancelled = true;
    };
  }, [open, activeTag]);

  useEffect(() => {
    if (!selected) {
      setBacklinks([]);
      return;
    }
    let cancelled = false;
    api
      .backlinks(selected)
      .then((r) => {
        if (!cancelled) setBacklinks(r);
      })
      .catch(() => {
        if (!cancelled) setBacklinks([]);
      });
    return () => {
      cancelled = true;
    };
  }, [selected]);

  const selectNote = useCallback(
    async (uuid: string) => {
      try {
        const note = await api.readNote(uuid);
        setSelected(uuid);
        setDoc(note.content);
        setDirty(false);
        setError("");
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  // Immediate (non-debounced) save of the open document. Runs from the
  // autosave timer and from the hide/exit hooks below, so pending keystrokes
  // are never lost when the window goes away.
  const flushSave = useCallback(async () => {
    if (saveTimer.current) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    const uuid = selectedRef.current;
    if (!uuid) return;
    if (!dirtyRef.current) return;
    const n = notesRef.current.find((x) => x.uuid === uuid);
    if (!n) return;
    try {
      await api.saveNote(n.path, docRef.current);
      setDirty(false);
      addLog(`saved: ${n.path}`);
      void refresh();
    } catch (e) {
      setError(String(e));
      addLog(`save failed: ${n.path}`);
    }
  }, [addLog, refresh]);

  const scheduleSave = useCallback(() => {
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      void flushSave();
    }, autosaveMsRef.current);
  }, [flushSave]);

  const onDoc = useCallback(
    (s: string) => {
      setDoc(s);
      setDirty(true);
      scheduleSave();
    },
    [scheduleSave],
  );

  // Flush pending edits when the page goes away: app backgrounded (Android
  // freezes the webview — the autosave timer never fires in the meantime),
  // tab hidden, or the window closing.
  useEffect(() => {
    const onHide = () => {
      if (document.visibilityState === "hidden") void flushSave();
    };
    const onPageHide = () => {
      void flushSave();
    };
    document.addEventListener("visibilitychange", onHide);
    window.addEventListener("pagehide", onPageHide);
    return () => {
      document.removeEventListener("visibilitychange", onHide);
      window.removeEventListener("pagehide", onPageHide);
    };
  }, [flushSave]);

  // Window close is intercepted on the Rust side (the CloseRequested window
  // event handler in lib.rs): the close is prevented, "sn-before-close" is
  // emitted, and the window is force-destroyed after a timeout if we never
  // confirm. Flush here, then confirm with force_close so the last
  // keystrokes always land on disk.
  useEffect(() => {
    let un: (() => void) | undefined;
    void listen("sn-before-close", () => {
      void (async () => {
        await flushSave();
        await api.forceClose().catch(() => {});
      })();
    }).then((u) => {
      un = u;
    });
    return () => {
      void un?.();
    };
  }, [flushSave]);

  const openWikiTarget = useCallback(
    (target: string) => {
      const clean = target.split("#")[0].trim().toLowerCase();
      if (!clean) return;
      const byPath = notesRef.current.find(
        (x) => x.path.toLowerCase() === clean,
      );
      const byPathMd = notesRef.current.find(
        (x) => x.path.toLowerCase() === `${clean}.md`,
      );
      const base = clean.split("/").pop() ?? clean;
      const byBase = notesRef.current.filter(
        (x) => (x.path.split("/").pop() ?? "").toLowerCase() === base,
      );
      const byBaseMd = notesRef.current.filter(
        (x) => (x.path.split("/").pop() ?? "").toLowerCase() === `${base}.md`,
      );
      const n =
        byPath ??
        byPathMd ??
        (byBase.length === 1 ? byBase[0] : null) ??
        (byBaseMd.length === 1 ? byBaseMd[0] : null);
      if (n) void selectNote(n.uuid);
      else addLog(`wikilink: note not found: ${target}`);
    },
    [selectNote, addLog],
  );

  const openTag = useCallback(
    (tag: string) => {
      setActiveTag(tag);
      setQuery("");
    },
    [],
  );

  const openVault = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      const res = await api.openVault(pathInput);
      localStorage.setItem("sn.vaultPath", pathInput);
      setOpen(res);
      setNotes(res.notes);
      setPeers(res.peers);
      setFolders(res.folders);
      // populate the sync footer (p2p settings + relay state)
      void refresh();
      addLog(
        `vault opened: ${res.path} (P2P port ${res.p2p_port}, device ${res.device_name})`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [pathInput, addLog]);

  const createNote = useCallback(async () => {
    const name = newName.trim();
    if (!name) return;
    const path = name.endsWith(".md") ? name : `${name}.md`;
    try {
      await api.saveNote(path, `# ${path.replace(/\.md$/, "")}\n`);
      setNewName("");
      await refresh();
      const n = notesRef.current.find((x) => x.path === path);
      if (n) void selectNote(n.uuid);
    } catch (e) {
      setError(String(e));
    }
  }, [newName, refresh, selectNote]);

  const doDelete = useCallback(async () => {
    const uuid = selectedRef.current;
    if (!uuid) return;
    const n = notesRef.current.find((x) => x.uuid === uuid);
    if (!n) return;
    if (!window.confirm(`Delete ${n.path}?`)) return;
    try {
      await api.deleteNote(uuid);
      setSelected(null);
      setDoc("");
      addLog(`deleted: ${n.path}`);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [addLog, refresh]);

  const doRename = useCallback(async () => {
    const uuid = selectedRef.current;
    if (!uuid) return;
    const n = notesRef.current.find((x) => x.uuid === uuid);
    if (!n) return;
    const np = window.prompt("New path (relative to the vault):", n.path);
    if (!np || np === n.path) return;
    try {
      await api.renameNote(uuid, np);
      addLog(`renamed: ${n.path} -> ${np}`);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [addLog, refresh]);

  const doCreateFolder = useCallback(
    async (parentDir: string) => {
      const name = window.prompt(
        parentDir ? `New folder in ${parentDir}:` : "New folder:",
      );
      if (!name || !name.trim()) return;
      const clean = name.trim();
      if (clean.includes("/") || clean.includes("\\") || clean === "..") {
        setError("Folder names cannot contain / or \\");
        return;
      }
      const path = parentDir ? `${parentDir}/${clean}` : clean;
      try {
        const created = await api.createFolder(path);
        addLog(`folder created: ${created}`);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doRenameFolder = useCallback(
    async (path: string) => {
      const np = window.prompt("New folder path:", path);
      if (!np || np.trim() === path) return;
      try {
        const n = await api.renameFolder(path, np.trim());
        addLog(`folder renamed: ${path} -> ${np.trim()} (${n} notes)`);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doDeleteFolder = useCallback(
    async (path: string) => {
      const count = notesRef.current.filter((n) =>
        n.path.startsWith(`${path}/`),
      ).length;
      if (
        !window.confirm(
          `Delete folder "${path}"${count ? ` and its ${count} note(s)` : ""}?`,
        )
      )
        return;
      try {
        const deleted = await api.deleteFolder(path);
        addLog(`folder deleted: ${path} (${deleted.length} notes)`);
        // close the editor if the open note lived inside
        const sel = selectedRef.current;
        if (
          sel &&
          deleted.some((d) => d.uuid === sel)
        ) {
          setSelected(null);
          setDoc("");
        }
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doMoveFolder = useCallback(
    async (oldPath: string, destDir: string) => {
      const base = oldPath.split("/").pop() ?? oldPath;
      const newPath = destDir ? `${destDir}/${base}` : base;
      if (newPath === oldPath) return; // dropped where it already is
      try {
        const n = await api.renameFolder(oldPath, newPath);
        addLog(`folder moved: ${oldPath} -> ${newPath} (${n} notes)`);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doMoveNote = useCallback(
    async (uuid: string, destDir: string | null) => {
      const n = notesRef.current.find((x) => x.uuid === uuid);
      if (!n) return;
      const base = n.path.split("/").pop() ?? n.path;
      const newDir = destDir ?? "";
      const curDir = n.path.slice(0, n.path.length - base.length).replace(/\/$/, "");
      if (newDir === curDir) return; // dropped where it already is
      const newPath = newDir ? `${newDir}/${base}` : base;
      try {
        await api.renameNote(uuid, newPath);
        addLog(`moved: ${n.path} -> ${newPath}`);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doDeleteNoteByUuid = useCallback(
    async (uuid: string) => {
      const n = notesRef.current.find((x) => x.uuid === uuid);
      if (!n) return;
      if (!window.confirm(`Delete ${n.path}?`)) return;
      try {
        await api.deleteNote(uuid);
        if (selectedRef.current === uuid) {
          setSelected(null);
          setDoc("");
        }
        addLog(`deleted: ${n.path}`);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [addLog, refresh],
  );

  const doSync = useCallback(
    async (peer: PeerInfo) => {
      setBusy(true);
      try {
        const r = await api.syncWith(peer);
        addLog(
          `P2P ${r.peer_name}: sent ${r.sent}, applied ${r.applied}, skipped ${r.skipped}`,
        );
      } catch (e) {
        addLog(`P2P ${peer.name}: ${e}`);
      } finally {
        setBusy(false);
      }
    },
    [addLog],
  );

  const doSyncAll = useCallback(async () => {
    setBusy(true);
    try {
      const rs = await api.syncAll();
      if (!rs.length) {
        // Empty report means either no peers at all or peers found but none
        // trusted (untrusted peers are skipped). Point at the real cause.
       addLog(
           peers.length
            ? 'sync: peers found, but none is trusted — press "Trust"'
            : "sync: no peers found (mDNS)",
        );
      }
      for (const r of rs) {
        addLog(
          `P2P ${r.peer_name}: sent ${r.sent}, applied ${r.applied}, skipped ${r.skipped}`,
        );
      }
    } catch (e) {
      addLog(`sync: ${e}`);
    } finally {
      setBusy(false);
    }
  }, [addLog, peers]);

 

  if (!open) {
    return (
      <div className="welcome">
        <h1>SpiderNotes</h1>
        <p className="muted">
          Markdown notes with P2P sync across your home network
        </p>
        <div className="welcome-card">
          <label>Vault path:</label>
          <input
            className="inp"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && openVault()}
          />
          <button disabled={busy} onClick={openVault}>
            Open / create vault
          </button>
          <p className="muted">
            Empty field — default path (on a phone the vault lives in the
            app's internal storage)
          </p>
          {error && <div className="error">{error}</div>}
        </div>
      </div>
    );
  }

  return (
    <div className="layout">
     <header className="topbar">
        <div className="brand">
          SpiderNotes <span className="muted">· {open.device_name}</span>
        </div>
        <div className="top-actions">
          <span className={dirty ? "status dirty" : "status"}>
            {dirty ? "unsaved" : "saved"}
          </span>
        </div>
      </header>

      <div className="body">
        {/* Always mounted: the .open class drives the slide-in/out
            transition (unmounting would kill the closing animation). */}
        <aside
          className={showSidebar ? "sidebar open" : "sidebar"}
          style={{ "--sb-w": `${sidebarW}px` } as React.CSSProperties}
          onContextMenu={(e) => {
            // Notes/folders have their own menus (stopPropagation), inputs
            // keep the native one (copy/paste); anywhere else in the panel
            // opens the root menu (New folder)
            const t = e.target as Element | null;
            if (
              t?.closest("input, select, textarea, .tree-item, .tree-dir, .ctx-menu")
            )
              return;
            e.preventDefault();
            window.dispatchEvent(
              new CustomEvent("sn-root-menu", {
                detail: { x: e.clientX, y: e.clientY },
              })
            );
          }}
        >
          {/* Top row: search + note close + settings gear, in the panel's
              top-right corner (on phones the panel covers the top bar,
              so the gear lives here, not in the header). */}
          <div className="side-top">
            <div className="search">
              <SearchIcon className="search-ico" />
              <input
                className="inp"
                placeholder="Search in text..."
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>
            <button
              className="icon-btn"
              title={
                selected
                  ? "Close the selected note"
                  : "No note selected"
              }
              disabled={!selected}
              onClick={() => {
                setSelected(null);
                setDoc("");
              }}
            >
              <CloseIcon />
            </button>
            <button
              className={showNet ? "icon-btn active" : "icon-btn"}
              title="Network & log"
              onClick={() => setShowNet((v) => !v)}
            >
              <GearIcon />
            </button>
          </div>
          {tags.length > 0 && (
            <div className="tags">
              {tags.map((t) => (
                <button
                  key={t.tag}
                  className={
                    activeTag === t.tag ? "tag active" : "tag"
                  }
                  onClick={() =>
                    setActiveTag(activeTag === t.tag ? null : t.tag)
                  }
                  title={`${t.count} notes`}
                >
                  #{t.tag} <span className="muted">{t.count}</span>
                </button>
              ))}
            </div>
          )}
          <div className="pad newnote">
            <input
              className="inp"
              placeholder="note name"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && createNote()}
            />
            <button
              className="icon-btn"
              title="Create note"
              onClick={createNote}
              disabled={!newName.trim()}
            >
              <PlusIcon />
            </button>
          </div>
          <div className="tree-wrap">
            <NotesTree
              notes={
                query.trim()
                  ? searchResults ?? []
                  : activeTag
                    ? tagNotes ?? []
                    : notes
              }
              folders={query.trim() || activeTag ? [] : folders}
              allFolders={folders}
              selected={selected}
              onSelect={selectNote}
              onCreateFolder={doCreateFolder}
              onRenameFolder={doRenameFolder}
              onDeleteFolder={doDeleteFolder}
               onMoveNote={(uuid, dir) => void doMoveNote(uuid, dir)}
                 onDeleteNote={(uuid) => void doDeleteNoteByUuid(uuid)}
                 onMoveFolder={(path, dir) => void doMoveFolder(path, dir)}
                 deviceNames={deviceNames}
                 conflictCount={conflictCount}
                 onConflict={openConflict}
             />
            {(query.trim() || activeTag) && (
              <button
                className="wide"
                onClick={() => {
                  setQuery("");
                  setActiveTag(null);
                }}
              >
                Reset filter
              </button>
            )}
          </div>
          {selected && (
            <div className="pad note-actions">
              <button className="danger" onClick={doDelete}>
                Delete
              </button>
              <button onClick={doRename}>Rename</button>
            </div>
          )}
          {selected && backlinks.length > 0 && (
            <div className="backlinks">
              <h3>Backlinks ({backlinks.length})</h3>
              {backlinks.map((b) => (
                 <button
                   key={b.note.uuid}
                   className="backlink"
                   onClick={() => selectNote(b.note.uuid)}
                   title={b.note.path}
                 >
                   {b.note.path}
                   {b.count > 1 && <span className="muted"> ×{b.count}</span>}
                 </button>
               ))}
             </div>
           )}
           {/* Sync status footer: connection mode + last sync time + manual sync */}
           <div className="sync-foot">
             {ssOn ? (
               <GlobeIcon className="sync-ico" />
             ) : p2p?.p2p_running ? (
               <WifiIcon className="sync-ico" />
             ) : (
               <WifiOffIcon className="sync-ico" />
             )}
             <span
               className="muted sync-text"
               title={
                 ssOn
                   ? "Syncing through the relay server"
                   : p2p?.p2p_running
                     ? "Direct sync on the local network (mDNS)"
                     : "No sync connection"
               }
             >
               {ssOn ? "Sync server" : p2p?.p2p_running ? "Local network" : "Offline"}
               {p2p?.last_sync ? ` · ${formatWhen(p2p.last_sync * 1000)}` : ""}
             </span>
             <button
               className="icon-btn"
               title="Sync with all trusted devices now"
               disabled={busy || !peers.length}
               onClick={() => void doSyncAll()}
             >
               <SyncIcon />
             </button>
           </div>
         </aside>

         {/* Drag the divider to resize the notes list (double-click =
             reset); desktop only, hidden while the panel is closed. */}
         <div
           className="resize-handle"
           title="Drag to resize the notes list"
           onPointerDown={startResize}
           onDoubleClick={resetSidebarW}
         />

        <main className="main">
          {/* Always-visible rail: hides/shows the notes list. Sits next to
              the sidebar and glides to the window's left edge when it is
              hidden. */}
          <button
            className="side-rail"
            title={
              showSidebar
                ? "Hide notes list"
                : "Show notes list"
            }
            onClick={() => setShowSidebar((v) => !v)}
          >
            {showSidebar ? <ChevronLeftIcon /> : <ChevronRightIcon />}
          </button>
          <div className="main-content">
            {selected ? (
            <div className="editor-area">
              {/* Preview / Raw Markdown toggle now lives in the Visual
                  settings section (gear -> Visual settings). */}
              <Editor
                doc={doc}
                onDoc={onDoc}
                notes={notes}
                live={live}
                theme={theme}
                lineNumbers={lineNumbers}
                onOpenNote={openWikiTarget}
                onOpenTag={openTag}
              />
            </div>
          ) : (
            <div className="empty">
               <NoteIcon className="empty-ico" />
               <div className="empty-text">
                 {showSidebar
                   ? "Select a note on the left or create a new one"
                   : "Open the notes list from the left (swipe — on a phone)"}
               </div>
             </div>
          )}
            {error && <div className="error">{error}</div>}
          </div>
        </main>

        {showNet && (
          <PeersPanel
            deviceId={open.device_id}
            deviceName={open.device_name}
            peers={peers}
            busy={busy}
            onSync={doSync}
            onSyncAll={doSyncAll}
            autosaveSecs={autosaveSecs}
            onAutosaveSecs={setAutosaveSecs}
            buildSha={__BUILD_SHA__.slice(0, 7)}
            vaultPath={open.path}
            onLog={addLog}
            log={log}
            theme={theme}
            onTheme={setTheme}
            fontSize={fontSize}
            onFontSize={setFontSize}
            lineNumbers={lineNumbers}
            onLineNumbers={setLineNumbers}
            live={live}
            onLive={setLive}
            noteFont={noteFont}
            onNoteFont={setNoteFont}
          onClose={() => setShowNet(false)}
           />
        )}

        {conflictView && (
          <div
            className="conflict-overlay"
            onClick={(e) => {
              if (e.target === e.currentTarget) closeConflict();
            }}
          >
            <div className="conflict-card">
              <div className="conflict-head">
                <WarnIcon className="conflict-warn" size={16} />
                <h2>Sync conflict</h2>
                <button
                  className="icon-btn"
                  title="Close"
                  onClick={closeConflict}
                >
                  <CloseIcon />
                </button>
              </div>
              <div className="muted small">
                {conflictView.note_path} — {conflictView.peer_name} ·{" "}
                {formatWhen(conflictView.created_ms)}
              </div>
              <p className="muted small conflict-hint">
                Both devices edited this note in ways that cannot be combined
                automatically. Pick the version to keep — nothing is lost
                until you decide.
              </p>
              <div className="conflict-version">
                <div className="conflict-chip">This device</div>
                <pre className="conflict-pre">
                  {conflictParts ? conflictParts.ours : "loading…"}
                </pre>
                <button
                  disabled={!conflictParts}
                  onClick={() => void doResolveConflict(false)}
                >
                  Keep this version
                </button>
              </div>
              <div className="conflict-version">
                <div className="conflict-chip">{conflictView.peer_name}</div>
                <pre className="conflict-pre">
                  {conflictParts ? conflictParts.theirs : "loading…"}
                </pre>
                <button
                  disabled={!conflictParts}
                  onClick={() => void doResolveConflict(true)}
                >
                  Keep this version
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}


