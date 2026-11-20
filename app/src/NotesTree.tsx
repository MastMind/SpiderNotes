import { useMemo } from "react";
import type { NoteSummary } from "./types";

interface TreeItem {
  name: string;
  note: NoteSummary | null;
  children: Record<string, TreeItem>;
}

function buildTree(notes: NoteSummary[]): Record<string, TreeItem> {
  const root: Record<string, TreeItem> = {};
  for (const n of notes) {
    const parts = n.path.split("/");
    let level = root;
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      const last = i === parts.length - 1;
      if (!level[part]) {
        level[part] = { name: part, note: last ? n : null, children: {} };
      }
      if (!last) level = level[part].children;
    }
  }
  return root;
}

function Tree({
  items,
  selected,
  onSelect,
}: {
  items: Record<string, TreeItem>;
  selected: string | null;
  onSelect: (uuid: string) => void;
}) {
  const entries = Object.values(items).sort(
    (a, b) =>
      Number(!!a.note) - Number(!!b.note) || a.name.localeCompare(b.name),
  );
  return (
    <ul className="tree">
      {entries.map((it) =>
        it.note ? (
          <li key={it.note.uuid}>
            <button
              className={selected === it.note.uuid ? "tree-item sel" : "tree-item"}
              onClick={() => onSelect(it.note!.uuid)}
              title={it.note.path}
            >
              {it.name}
            </button>
          </li>
        ) : (
          <li key={it.name}>
            <details open>
              <summary className="tree-dir">{it.name}</summary>
              <Tree items={it.children} selected={selected} onSelect={onSelect} />
            </details>
          </li>
        ),
      )}
    </ul>
  );
}

export function NotesTree({
  notes,
  query,
  selected,
  onSelect,
}: {
  notes: NoteSummary[];
  query: string;
  selected: string | null;
  onSelect: (uuid: string) => void;
}) {
  const tree = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? notes.filter((n) => n.path.toLowerCase().includes(q))
      : notes;
    return buildTree(filtered);
  }, [notes, query]);

  if (!notes.length) {
    return <div className="muted pad">No notes yet</div>;
  }
  return <Tree items={tree} selected={selected} onSelect={onSelect} />;
}
