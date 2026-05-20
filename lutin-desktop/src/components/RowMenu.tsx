import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import styles from "./Sidebar.module.css";

export interface RowMenuItem {
  label: string;
  onSelect: () => void;
  /** Items flagged `danger` render in the error color. */
  danger?: boolean;
}

interface Props {
  pos: { x: number; y: number };
  items: RowMenuItem[];
  onClose: () => void;
}

/** Tiny portal-rendered context menu for sidebar rows. Closes on
 *  outside click, Escape, or a window resize/scroll — the latter
 *  because we don't reposition on the fly. */
export function RowMenu({ pos, items, onClose }: Props) {
  const ref = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  if (typeof document === "undefined") return null;
  return createPortal(
    <ul
      ref={ref}
      role="menu"
      className={styles.rowMenu}
      style={{ left: pos.x, top: pos.y }}
    >
      {items.map((it) => (
        <li key={it.label} role="none">
          <button
            type="button"
            role="menuitem"
            className={`${styles.rowMenuItem} ${it.danger ? styles.rowMenuItemDanger : ""}`}
            onClick={it.onSelect}
          >
            {it.label}
          </button>
        </li>
      ))}
    </ul>,
    document.body,
  );
}
