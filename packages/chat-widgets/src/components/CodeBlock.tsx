import { useMemo } from "react";
import hljs from "highlight.js/lib/common";

const EXT_LANG: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  rs: "rust",
  go: "go",
  rb: "ruby",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  hpp: "cpp",
  cs: "csharp",
  php: "php",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "bash",
  json: "json",
  toml: "ini",
  yml: "yaml",
  yaml: "yaml",
  xml: "xml",
  html: "xml",
  htm: "xml",
  svg: "xml",
  css: "css",
  scss: "scss",
  md: "markdown",
  sql: "sql",
  dockerfile: "dockerfile",
  lua: "lua",
};

/** Read-tool output prefixes each line with `  N→` (or `  N\t`).
 *  Strip them so the gutter doesn't render doubled numbers, and
 *  recover the starting line number so the gutter is still accurate
 *  when the read used a non-zero offset. Returns the input unchanged
 *  when fewer than half of the first-page lines match the pattern,
 *  so plain content (e.g. an empty file) renders correctly. */
export function parseReadOutput(text: string): { content: string; startLine: number } {
  if (text.length === 0) return { content: text, startLine: 1 };
  const lines = text.split("\n");
  const probe = lines.slice(0, Math.min(20, lines.length));
  const re = /^\s*(\d+)[\t→](.*)$/;
  let matched = 0;
  let firstNo: number | null = null;
  for (const ln of probe) {
    if (ln.length === 0) continue;
    const m = re.exec(ln);
    if (m) {
      matched++;
      if (firstNo === null) firstNo = parseInt(m[1], 10);
    }
  }
  if (matched < Math.max(1, Math.floor(probe.filter((l) => l.length > 0).length / 2))) {
    return { content: text, startLine: 1 };
  }
  const stripped = lines.map((ln) => {
    const m = re.exec(ln);
    return m ? m[2] : ln;
  });
  return { content: stripped.join("\n"), startLine: firstNo ?? 1 };
}

export function langFromPath(path: string | undefined): string {
  if (!path) return "plaintext";
  const base = path.split("/").pop() || path;
  if (base.toLowerCase() === "dockerfile") return "dockerfile";
  const ext = base.includes(".") ? base.split(".").pop()!.toLowerCase() : "";
  const lang = EXT_LANG[ext];
  if (lang && hljs.getLanguage(lang)) return lang;
  return "plaintext";
}

export interface CodeBlockProps {
  code: string;
  language?: string;
  startLine?: number;
  className?: string;
}

/** Highlighted code with gutter line numbers. Pure render — no
 *  collapse/expand state; callers wrap this in their own container. */
export function CodeBlock({ code, language, startLine = 1, className }: CodeBlockProps) {
  const html = useMemo(() => {
    const lang = language && hljs.getLanguage(language) ? language : "plaintext";
    return hljs.highlight(code, { language: lang }).value;
  }, [code, language]);
  const lineCount = code.length === 0 ? 0 : code.split("\n").length;
  const gutter = useMemo(() => {
    const lines: string[] = [];
    for (let i = 0; i < lineCount; i++) lines.push(String(startLine + i));
    return lines.join("\n");
  }, [lineCount, startLine]);
  return (
    <pre className={`lutin-code ${className ?? ""}`}>
      <code className="lutin-code__gutter" aria-hidden="true">{gutter}</code>
      <code
        className={`lutin-code__src hljs language-${language ?? "plaintext"}`}
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </pre>
  );
}
