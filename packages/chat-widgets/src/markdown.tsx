import { useCallback, useMemo } from "react";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import { Marked } from "marked";

// Force every rendered <a> to open out-of-frame. Without this, a click
// on a markdown link inside the chat iframe navigates the iframe to a
// blank page (the iframe's document _is_ the chat UI). target="_blank"
// + rel keeps the link safe; the click handler below is a belt-and-
// braces fallback for environments where target="_blank" is blocked.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

// One configured Marked instance for the whole package: GFM + breaks.
// Code-block rendering (header + syntax highlighting via highlight.js)
// is handled by the renderer override below — we use the "common"
// hljs bundle (~35 popular languages) to keep ship size down.
const md = new Marked();
md.setOptions({ gfm: true, breaks: true });

md.use({
  renderer: {
    code({ text, lang }) {
      const tag = (lang || "").trim().split(/\s+/)[0] || "plaintext";
      const known = hljs.getLanguage(tag) ? tag : "plaintext";
      const highlighted = hljs.highlight(text, { language: known }).value;
      const safeLang = escapeHtml(tag);
      return (
        `<div class="lutin-md__pre">` +
        `<div class="lutin-md__pre-head"><span class="lutin-md__pre-lang">${safeLang}</span></div>` +
        `<pre><code class="hljs language-${safeLang}">${highlighted}</code></pre>` +
        `</div>`
      );
    },
  },
});

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => {
    switch (c) {
      case "&":
        return "&amp;";
      case "<":
        return "&lt;";
      case ">":
        return "&gt;";
      case '"':
        return "&quot;";
      default:
        return "&#39;";
    }
  });
}

export interface MarkdownProps {
  text: string;
  className?: string;
}

export function Markdown({ text, className }: MarkdownProps) {
  const html = useMemo(() => {
    const raw = md.parse(text ?? "", { async: false }) as string;
    return DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      // hljs emits <span class="hljs-..."> nodes — keep classes through purify.
      ADD_ATTR: ["class"],
    });
  }, [text]);

  const onClick = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    // Walk up from the click target — the user may click a child of <a>
    // (e.g. <strong> inside the link text).
    let el = e.target as HTMLElement | null;
    while (el && el !== e.currentTarget) {
      if (el.tagName === "A") {
        const href = (el as HTMLAnchorElement).getAttribute("href");
        if (href && /^(https?:|mailto:)/i.test(href)) {
          e.preventDefault();
          // In a sandboxed iframe `target="_blank"` and `window.open`
          // are both blocked, and the default click would navigate
          // the iframe document itself (replacing the chat UI with
          // a blank page). Ask the host to open the URL externally.
          if (window.parent && window.parent !== window) {
            window.parent.postMessage(
              { type: "lutin-open-url", url: href },
              "*",
            );
          } else {
            window.open(href, "_blank", "noopener,noreferrer");
          }
        }
        return;
      }
      el = el.parentElement;
    }
  }, []);

  const cls = ["lutin-md", className].filter(Boolean).join(" ");
  return (
    <div
      className={cls}
      onClick={onClick}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
