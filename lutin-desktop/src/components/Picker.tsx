import { useEffect, useMemo, useRef, useState } from "react";
import { fuzzyRank } from "../fuzzy";
import { Modal } from "./Modal";
import styles from "./Modal.module.css";

export interface PickerItem {
  id: string;
  label: string;
  sub?: string;
}

export interface PickerProps<T extends PickerItem> {
  title: string;
  placeholder?: string;
  items: T[];
  onSelect: (item: T) => void;
  onClose: () => void;
  /// Optional renderer for the right-side meta column. Falls back to
  /// `item.sub` when omitted.
  renderSub?: (item: T) => React.ReactNode;
  /// When provided, opens the modal in two-column layout: list on
  /// the left, this renderer's output on the right. `null` arg means
  /// nothing is focused (empty list / no matches) — return a hint or
  /// just `null` to let the picker show its default empty state.
  renderPreview?: (item: T | null) => React.ReactNode;
}

export function Picker<T extends PickerItem>({
  title,
  placeholder,
  items,
  onSelect,
  onClose,
  renderSub,
  renderPreview,
}: PickerProps<T>) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const listRef = useRef<HTMLUListElement | null>(null);

  const ranked = useMemo(
    () => fuzzyRank(items, query, (it) => it.label + " " + (it.sub ?? "")),
    [items, query],
  );

  useEffect(() => {
    setCursor(0);
  }, [query]);

  useEffect(() => {
    const root = listRef.current;
    if (!root) return;
    const el = root.children[cursor] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const visible = ranked.map((r) => r.item);
  const focused = visible[cursor] ?? null;

  const choose = (item: T) => {
    onSelect(item);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const down = e.key === "ArrowDown" || (e.ctrlKey && e.key === "n") || (e.key === "Tab" && !e.shiftKey);
    const up = e.key === "ArrowUp" || (e.ctrlKey && e.key === "p") || (e.key === "Tab" && e.shiftKey);
    if (down) {
      e.preventDefault();
      const n = visible.length;
      if (n > 0) setCursor((c) => (c + 1) % n);
    } else if (up) {
      e.preventDefault();
      const n = visible.length;
      if (n > 0) setCursor((c) => (c - 1 + n) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = visible[cursor];
      if (item) choose(item);
    }
  };

  const list = visible.length === 0 ? (
    <div className={styles.empty}>No matches.</div>
  ) : (
    <ul ref={listRef} className={styles.list}>
      {visible.map((item, i) => (
        <li
          key={item.id}
          className={`${styles.option} ${i === cursor ? styles.optionActive : ""}`}
          onMouseEnter={() => setCursor(i)}
          onClick={() => choose(item)}
        >
          <span>{item.label}</span>
          <span className={styles.optionSub}>
            {renderSub ? renderSub(item) : item.sub}
          </span>
        </li>
      ))}
    </ul>
  );

  return (
    <Modal onClose={onClose} wide={!!renderPreview}>
      <div className={styles.header}>
        <span className={styles.title}>{title}</span>
      </div>
      <input
        className={styles.input}
        placeholder={placeholder ?? "Type to filter…"}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onKeyDown}
        autoFocus
      />
      {renderPreview ? (
        <div className={styles.split}>
          <div className={styles.splitLeft}>{list}</div>
          <div className={styles.splitRight}>
            {renderPreview(focused) ?? (
              <div className={styles.previewEmpty}>Nothing to preview.</div>
            )}
          </div>
        </div>
      ) : (
        list
      )}
    </Modal>
  );
}
