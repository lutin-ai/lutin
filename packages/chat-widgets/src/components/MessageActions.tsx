import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

// Per-message right-click affordances. Each bubble owns its own menu
// and edit state, so multiple bubbles can't share an open menu — that
// avoids a global portal/coordinator just for three menu items.

export interface MessageActions {
  /** Replace the message text. Implementations decide whether to
   *  truncate downstream history; this widget just submits the new
   *  text. Omit to hide the Edit item. */
  onEdit?: (id: string, newText: string) => void;
  /** Drop the message in place (no truncation). */
  onDelete?: (id: string) => void;
  /** Truncate this message and everything after it. */
  onDeleteFromHere?: (id: string) => void;
}

interface MenuItem {
  label: string;
  onSelect: () => void;
}

interface UseMessageMenuArgs {
  id: string | undefined;
  text: string;
  actions?: MessageActions;
  /** Extra items appended after the built-ins (Copy/Edit/Delete/…).
   *  Used by tool cards to add a "Collapse" toggle. */
  extraItems?: MenuItem[];
  /** When true, the menu adds a "Show/Hide info" item that toggles
   *  `infoOpen`. Bubbles spread `dataAttrs` on their root so the
   *  CSS hover rules can pin the metrics chip while info is open. */
  hasMeta?: boolean;
}

export function useMessageMenu({ id, text, actions, extraItems, hasMeta }: UseMessageMenuArgs) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const [host, setHost] = useState<HTMLElement | null>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [infoOpen, setInfoOpen] = useState(false);

  const open = useCallback(
    (e: React.MouseEvent) => {
      // Only act when there's something to do AND we have a stable id.
      if (!id) return;
      if (!actions && !canCopy() && !(extraItems && extraItems.length > 0) && !hasMeta) return;
      e.preventDefault();
      setPos({ x: e.clientX, y: e.clientY });
      // Portal target: the nearest `.lutin-chat` ancestor. We can't
      // portal to `document.body` because the chat's CSS variables
      // (--chat-surface, --chat-text, …) are scoped to `.lutin-chat` —
      // outside that scope the menu renders transparent on default
      // body color. The `.lutin-chat` root has no `transform`, so
      // `position: fixed` viewport coords still resolve correctly
      // (the transform that breaks fixed-positioning lives on the
      // virtualizer's row wrappers, deeper in the tree).
      const target = e.currentTarget as HTMLElement;
      setHost(target.closest(".lutin-chat") as HTMLElement | null);
    },
    [id, actions, extraItems, hasMeta],
  );

  const close = useCallback(() => setPos(null), []);

  const items: MenuItem[] = [];
  if (canCopy()) {
    items.push({
      label: "Copy",
      onSelect: () => {
        void navigator.clipboard.writeText(text);
        close();
      },
    });
  }
  if (actions?.onEdit && id) {
    items.push({
      label: "Edit",
      onSelect: () => {
        setDraft(text);
        setEditing(true);
        close();
      },
    });
  }
  if (actions?.onDelete && id) {
    items.push({
      label: "Delete",
      onSelect: () => {
        actions.onDelete!(id);
        close();
      },
    });
  }
  if (actions?.onDeleteFromHere && id) {
    items.push({
      label: "Delete from here",
      onSelect: () => {
        actions.onDeleteFromHere!(id);
        close();
      },
    });
  }
  if (hasMeta) {
    items.push({
      label: infoOpen ? "Hide info" : "Show info",
      onSelect: () => {
        setInfoOpen((v) => !v);
        close();
      },
    });
  }
  if (extraItems) {
    for (const it of extraItems) {
      // Wrap onSelect so the caller doesn't have to remember to close()
      // — every menu item dismisses the menu after firing.
      items.push({
        label: it.label,
        onSelect: () => {
          it.onSelect();
          close();
        },
      });
    }
  }

  const submitEdit = useCallback(() => {
    if (!actions?.onEdit || !id) return;
    const next = draft;
    setEditing(false);
    if (next !== text) actions.onEdit(id, next);
  }, [actions, draft, id, text]);

  const cancelEdit = useCallback(() => setEditing(false), []);

  return {
    onContextMenu: open,
    /** Spread on the root bubble element so CSS can pin the metrics
     *  chip visible while the user has "Show info" toggled on. */
    dataAttrs: infoOpen ? { "data-info": "open" as const } : {},
    infoOpen,
    menu:
      pos && items.length > 0 ? (
        <ContextMenu pos={pos} items={items} onClose={close} host={host} />
      ) : null,
    editing,
    editor:
      editing && actions?.onEdit ? (
        <InlineEditor
          value={draft}
          onChange={setDraft}
          onSave={submitEdit}
          onCancel={cancelEdit}
        />
      ) : null,
  };
}

function canCopy(): boolean {
  return typeof navigator !== "undefined" && !!navigator.clipboard;
}

function ContextMenu({
  pos,
  items,
  onClose,
  host,
}: {
  pos: { x: number; y: number };
  items: MenuItem[];
  onClose: () => void;
  host: HTMLElement | null;
}) {
  const ref = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const onDocDown = (e: MouseEvent) => {
      if (!ref.current) return;
      if (!ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDocDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // Portal target: the `.lutin-chat` root so the menu inherits the
  // chat's CSS variables (--chat-surface, --chat-text, …). Falling
  // back to `document.body` keeps the menu visible when no host is
  // resolvable, but it'll render unstyled — that's a developer bug
  // (the menu was opened from a bubble outside any `.lutin-chat`
  // ancestor) rather than a runtime path users should hit.
  if (typeof document === "undefined") return null;
  const target = host ?? document.body;
  return createPortal(
    <ul
      ref={ref}
      className="lutin-chat__context-menu"
      role="menu"
      style={{ left: pos.x, top: pos.y }}
    >
      {items.map((it) => (
        <li key={it.label} role="none">
          <button
            type="button"
            role="menuitem"
            className="lutin-chat__context-menu-item"
            onClick={it.onSelect}
          >
            {it.label}
          </button>
        </li>
      ))}
    </ul>,
    target,
  );
}

function InlineEditor({
  value,
  onChange,
  onSave,
  onCancel,
}: {
  value: string;
  onChange: (v: string) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  return (
    <div className="lutin-chat__msg-edit">
      <textarea
        ref={ref}
        className="lutin-chat__msg-edit-input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            onSave();
          }
        }}
        rows={Math.min(12, Math.max(2, value.split("\n").length))}
      />
      <div className="lutin-chat__msg-edit-actions">
        <button type="button" className="lutin-chat__msg-edit-cancel" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="lutin-chat__msg-edit-save" onClick={onSave}>
          Save
        </button>
      </div>
    </div>
  );
}
