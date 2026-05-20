import { useEffect, useRef } from "react";
import styles from "./Modal.module.css";

export interface ModalProps {
  onClose: () => void;
  children: React.ReactNode;
  /// Use the wider panel for two-column layouts (picker + side preview).
  wide?: boolean;
}

/// Centered overlay with backdrop. Esc closes; clicking the backdrop
/// closes; clicks inside the panel don't bubble. Focus moves to the
/// first focusable child on mount and is restored on unmount.
export function Modal({ onClose, children, wide = false }: ModalProps) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const prevFocus = useRef<HTMLElement | null>(null);

  useEffect(() => {
    prevFocus.current = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    // Focus the first focusable child (the input) after mount.
    const id = requestAnimationFrame(() => {
      const root = panelRef.current;
      if (!root) return;
      const target = root.querySelector<HTMLElement>(
        "input, textarea, [tabindex]",
      );
      target?.focus();
    });
    return () => {
      window.removeEventListener("keydown", onKey, true);
      cancelAnimationFrame(id);
      prevFocus.current?.focus?.();
    };
  }, [onClose]);

  return (
    <div className={styles.backdrop} onMouseDown={onClose}>
      <div
        ref={panelRef}
        className={`${styles.panel} ${wide ? styles.panelWide : ""}`}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}
