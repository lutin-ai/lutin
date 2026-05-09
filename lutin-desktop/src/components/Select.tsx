import {
  type CSSProperties,
  type ReactNode,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import styles from "./Select.module.css";

export type SelectOption = {
  value: string;
  label: ReactNode;
  disabled?: boolean;
};

// In-document dropdown. Native <select> on i3 + WebKitGTK opens an
// override-redirect popup window that i3 won't focus, so the first
// mouseup outside the click target dismisses the menu before a
// selection lands. Rendering the menu as a positioned div keeps the
// whole interaction inside the webview.
export function Select({
  value,
  onChange,
  options,
  disabled,
  className,
  style,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  disabled?: boolean;
  className?: string;
  style?: CSSProperties;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState<number>(() =>
    Math.max(0, options.findIndex((o) => o.value === value)),
  );
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLUListElement | null>(null);
  const listboxId = useId();

  const selected = options.find((o) => o.value === value);
  const triggerLabel = selected?.label ?? placeholder ?? "";

  useEffect(() => {
    if (!open) return;
    const onDocPointer = (e: PointerEvent) => {
      const t = e.target as Node | null;
      if (!t) return;
      if (triggerRef.current?.contains(t)) return;
      if (menuRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("pointerdown", onDocPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDocPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return;
    const idx = options.findIndex((o) => o.value === value);
    setHighlight(idx >= 0 ? idx : 0);
  }, [open, options, value]);

  useEffect(() => {
    if (!open) return;
    const node = menuRef.current?.querySelector<HTMLLIElement>(
      `[data-idx="${highlight}"]`,
    );
    node?.scrollIntoView({ block: "nearest" });
  }, [open, highlight]);

  const move = (delta: number) => {
    if (options.length === 0) return;
    let i = highlight;
    for (let step = 0; step < options.length; step++) {
      i = (i + delta + options.length) % options.length;
      if (!options[i].disabled) {
        setHighlight(i);
        return;
      }
    }
  };

  const commit = (idx: number) => {
    const opt = options[idx];
    if (!opt || opt.disabled) return;
    onChange(opt.value);
    setOpen(false);
    triggerRef.current?.focus();
  };

  return (
    <div className={`${styles.wrap} ${className ?? ""}`} style={style}>
      <button
        ref={triggerRef}
        type="button"
        className={styles.trigger}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listboxId : undefined}
        onClick={() => !disabled && setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (disabled) return;
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            if (!open) {
              setOpen(true);
              return;
            }
            move(e.key === "ArrowDown" ? 1 : -1);
          } else if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            if (open) commit(highlight);
            else setOpen(true);
          } else if (e.key === "Home" && open) {
            e.preventDefault();
            setHighlight(0);
          } else if (e.key === "End" && open) {
            e.preventDefault();
            setHighlight(options.length - 1);
          }
        }}
      >
        <span className={styles.triggerLabel}>
          {triggerLabel || <span className={styles.placeholder}>&nbsp;</span>}
        </span>
        <span className={styles.caret} aria-hidden>▾</span>
      </button>
      {open && (
        <ul
          ref={menuRef}
          id={listboxId}
          role="listbox"
          className={styles.menu}
          tabIndex={-1}
        >
          {options.map((opt, idx) => (
            <li
              key={opt.value + ":" + idx}
              role="option"
              data-idx={idx}
              aria-selected={opt.value === value}
              aria-disabled={opt.disabled || undefined}
              className={[
                styles.option,
                idx === highlight ? styles.optionHighlight : "",
                opt.value === value ? styles.optionSelected : "",
                opt.disabled ? styles.optionDisabled : "",
              ]
                .filter(Boolean)
                .join(" ")}
              onPointerEnter={() => !opt.disabled && setHighlight(idx)}
              onPointerDown={(e) => {
                e.preventDefault();
                commit(idx);
              }}
            >
              {opt.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
