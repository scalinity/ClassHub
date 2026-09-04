import { Marked } from "marked";
import katex from "katex";

import type { ClassInfo } from "@/lib/classes";
import { sentence } from "@/lib/utils";

/**
 * Model output is rendered into the app's own document, not a sandboxed frame,
 * so this module is the boundary that decides what markup an answer may become.
 *
 * Two things get through marked untouched that must not reach the DOM: raw HTML
 * blocks, and URLs. The `html` renderer neutralizes the first. The second needs
 * `link`/`image` overrides — marked only runs `encodeURI` on an href, which
 * leaves `javascript:` intact, and the app document holds the Tauri IPC bridge.
 */
const SAFE_URL = /^(?:https?:\/\/|mailto:|#|\/|\.{1,2}\/)/i;

/**
 * Anything at or below U+0020 comes out before the scheme test: a tab or a NUL
 * inside `java<tab>script:` hides the scheme from a prefix match but not from
 * the URL parser. The stripped form is what gets rendered.
 */
function safeUrl(href: string): string | null {
  const clean = Array.from(href)
    .filter((c) => c > " ")
    .join("");
  return SAFE_URL.test(clean) ? clean : null;
}

const md = new Marked({
  renderer: {
    /** The model's output is prose, not markup: raw HTML renders as literal text. */
    html: ({ text }) => escapeHtml(text),
    link({ href, title, tokens }) {
      const text = this.parser.parseInline(tokens);
      const url = safeUrl(href);
      // A rejected scheme keeps its text — dropping the label would hide from
      // the reader that the model offered a link at all.
      if (url === null) return text;
      const attr = title ? ` title="${escapeHtml(title)}"` : "";
      return `<a href="${escapeHtml(url)}"${attr}>${text}</a>`;
    },
    image({ href, title, text }) {
      const url = safeUrl(href);
      if (url === null) return escapeHtml(text);
      const attr = title ? ` title="${escapeHtml(title)}"` : "";
      return `<img src="${escapeHtml(url)}" alt="${escapeHtml(text)}"${attr}>`;
    },
  },
});

const VIEWABLE: Record<string, string> = {
  md: "md",
  markdown: "md",
  txt: "md",
  rmd: "rmd",
  r: "r",
  html: "html",
  htm: "html",
};

/**
 * Math, then markdown, then citations.
 *
 * TeX has to come out first: to a markdown parser `_` is emphasis and `\` is an
 * escape, so `$\dfrac{\sum x_i}{n}$` is mangled beyond rescue by the time the
 * HTML exists. Each span is lifted out, rendered by KaTeX, and put back after.
 * Code is matched by the same pass purely to be skipped — a `$` in an R snippet
 * is a column selector, not a formula.
 */
const MATH_OR_CODE =
  /(```[\s\S]*?```|`[^`\n]*`)|(\$\$[\s\S]+?\$\$|\\\[[\s\S]+?\\\])|(\$(?![\s$])[^\n$]+?(?<![\s\\])\$|\\\([\s\S]+?\\\))/g;

function liftMath(text: string): { source: string; rendered: string[] } {
  const rendered: string[] = [];
  const source = text.replace(
    MATH_OR_CODE,
    (whole: string, code?: string, display?: string, inline?: string) => {
      if (code !== undefined) return whole;
      const body = display ?? inline ?? "";
      const tex = body.startsWith("$$")
        ? body.slice(2, -2)
        : body.startsWith("$")
          ? body.slice(1, -1)
          : body.slice(2, -2);
      rendered.push(
        katex.renderToString(tex, {
          displayMode: display !== undefined,
          throwOnError: false,
          output: "html",
        }),
      );
      return `@@MATH${rendered.length - 1}@@`;
    },
  );
  return { source, rendered };
}

/**
 * Markdown, then citations: inline code naming a class-relative file (the
 * citation shape the system prompt asks for) becomes a button that opens it.
 */
export function renderAnswer(
  text: string,
  classes: readonly ClassInfo[],
): string {
  const { source, rendered } = liftMath(text);
  const parsed = md.parse(source, { async: false }) as string;
  const html = parsed.replace(
    /@@MATH(\d+)@@/g,
    (whole, index: string) => rendered[Number(index)] ?? whole,
  );
  // Fenced blocks carry a language class or sit inside <pre>; skip those.
  return html.replace(
    /(?<!<pre>)<code>([^<]+)<\/code>/g,
    (whole, inner: string) => {
      const path = decodeEntities(inner);
      const cls = classes.find((c) => path.startsWith(`${c.folderName}/`));
      if (!cls) return whole;
      const relPath = path.slice(cls.folderName.length + 1);
      const kind = VIEWABLE[relPath.split(".").pop()?.toLowerCase() ?? ""];
      if (!kind) return whole;
      return (
        `<button type="button" class="cite" style="--cite: var(--class-${cls.color})" ` +
        `data-class="${cls.id}" data-kind="${kind}" data-path="${escapeHtml(relPath)}" ` +
        `data-name="${escapeHtml(relPath.split("/").pop() ?? relPath)}">${inner}</button>`
      );
    },
  );
}

/**
 * The four-character form: `"` matters because this escapes values that land in
 * HTML attributes (the citation button's `data-path`), not only in text nodes.
 */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function decodeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}

/** Tool arguments read better as lines than as JSON. */
export function formatArgs(json: string): string {
  try {
    const entries = Object.entries(JSON.parse(json) as Record<string, unknown>);
    if (entries.length === 0) return "no arguments";
    return entries
      .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`)
      .join("\n");
  } catch {
    return json;
  }
}

/**
 * A model id as a name: the release date dropped, each word capitalised, and
 * a trailing run of numbers joined with dots — `claude-sonnet-5` reads
 * `Claude Sonnet 5`, `claude-opus-4-1-20250805` reads `Claude Opus 4.1`.
 */
export function shortModel(id: string): string {
  const words: string[] = [];
  for (const part of id.replace(/-\d{8}$/, "").split("-")) {
    const last = words[words.length - 1];
    if (/^\d+$/.test(part) && last !== undefined && /\d$/.test(last)) {
      words[words.length - 1] = `${last}.${part}`;
    } else {
      words.push(sentence(part));
    }
  }
  return words.join(" ");
}
