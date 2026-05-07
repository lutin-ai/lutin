import { useMemo } from "react";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import { Marked } from "marked";

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

  const cls = ["lutin-md", className].filter(Boolean).join(" ");
  return <div className={cls} dangerouslySetInnerHTML={{ __html: html }} />;
}
