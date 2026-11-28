import { useEffect, useRef, type ReactNode } from "react";

// Small modal dialog (overlay + card). Replaces window.prompt for the
// flows that need more than a single line (rename, create, unlock).
// Closes on Escape or overlay tap; the first input is focused on open.
export function Dialog(props: {
  title: string;
  onClose: () => void;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  const cardRef = useRef<HTMLDivElement>(null);
  // The effect must run ONCE on mount: callers pass an inline onClose
  // closure, so depending on it would re-run focus()+select() on every
  // keystroke — re-selecting the whole input so each typed character
  // erased the previous one.
  const closeRef = useRef(props.onClose);
  closeRef.current = props.onClose;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeRef.current();
    };
    window.addEventListener("keydown", onKey);
    // focus the first text-like control inside the dialog
    const el = cardRef.current?.querySelector<HTMLElement>(
      "input, select, textarea"
    );
    el?.focus();
    if (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement
    ) {
      el.select();
    }
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="dlg-overlay" onClick={props.onClose}>
      <div
        className="dlg"
        ref={cardRef}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="dlg-title">{props.title}</h2>
        {props.children && <div className="dlg-body">{props.children}</div>}
        {props.actions && (
          <div className="dlg-actions">{props.actions}</div>
        )}
      </div>
    </div>
  );
}
