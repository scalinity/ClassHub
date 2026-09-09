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

/**
 * Extensions a citation can open, and the viewer kind each becomes. The set
 * follows `VIEWABLE_KINDS` in `materials.ts`: a script and a CSV read in the
 * document register the same way a note does, and the material viewer has
 * rendered them since M20 — only this map had not caught up, so an answer
 * citing an R script or a dataset left dead text where a link belonged.
 */
const VIEWABLE: Record<string, string> = {
  md: "md",
  markdown: "md",
  txt: "md",
  rmd: "rmd",
  r: "r",
  py: "py",
  csv: "csv",
  html: "html",
  htm: "html",
};

/**
 * A transcript: a markdown file under the class's `Weeks/` folder, which is
 * where every filed lecture lives (SPEC §4). Only these carry `## HH:MM`
 * headings, so only these get a time turned into a link.
 */
function isTranscript(relPath: string): boolean {
  return (
    relPath.startsWith("Weeks/") && relPath.toLowerCase().endsWith(".md")
  );
}

/**
 * Where a citation stops carrying: a heading, which starts a new subject.
 *
 * Not the end of every paragraph, because that is not how the answer is
 * written. The model names the transcript once and then lists the moments
 * under it — "Four exam hints, all from `<path>`:" followed by a bullet per
 * `HH:MM` — so a citation has to reach the list it introduces.
 */
const SECTION_END = /(<\/h[1-6]>)/g;

/**
 * A time written as a clock time, which is not a transcript anchor: the app
 * writes those `11:59 pm` (SPEC §12) and a lecture anchor never carries a
 * meridiem. Checked on what follows the match.
 */
const CLOCK_TIME = /^\s*(?:&nbsp;)?\s*[ap]\.?m\.?/i;

/** A citation button, capturing what it points at. */
const CITE =
  /<button[^>]*class="cite"[^>]*data-path="([^"]*)"[^>]*>[\s\S]*?<\/button>/;

/**
 * Within one section: a whole fenced block, a whole citation button, any other
 * tag, or a bare `HH:MM` in text.
 *
 * Each alternative exists to be skipped. A `<pre>` is code, and a time in a
 * snippet is not an anchor into a lecture. Matching tags explicitly keeps a
 * time inside an attribute — a `data-path` naming a lecture at 09:00 — from
 * being rewritten, and matching a citation whole keeps a time that is already
 * a link from being wrapped twice.
 */
const PRE_CITE_TAG_OR_TIME =
  /(<pre[\s\S]*?<\/pre>)|(<button[^>]*class="cite"[^>]*data-path="([^"]*)"[^>]*>[\s\S]*?<\/button>)|(<[^>]+>)|(\b([01]?\d|2[0-3]):([0-5]\d)\b)/g;

/** The class id and colour a time's link borrows from the citation beside it. */
interface Cited {
  path: string;
  classId: string;
  color: string;
}

/**
 * What a citation points at, or `null` when it is not a transcript — a guide
 * or a note cited between two transcripts ends the carry rather than letting
 * the times under it reach back to the transcript above.
 */
function citedTranscript(button: string): Cited | null {
  const path = decodeEntities(CITE.exec(button)?.[1] ?? "");
  if (!isTranscript(path)) return null;
  return {
    path,
    classId: /data-class="(\d+)"/.exec(button)?.[1] ?? "",
    color: /--cite: var\(--class-([a-z]+)\)/.exec(button)?.[1] ?? "",
  };
}

/**
 * The one place a citation's markup is written. Both passes build the same
 * button — the path pass from a code span, the time pass from the transcript
 * beside it — and two hand-written copies would drift, silently breaking the
 * time pass's own regexes, which match what this emits.
 */
function citeButton(
  cls: { id: number | string; color: string },
  relPath: string,
  kind: string,
  label: string,
  anchor?: string,
): string {
  const name = relPath.split("/").pop() ?? relPath;
  return (
    `<button type="button" class="cite" style="--cite: var(--class-${cls.color})" ` +
    `data-class="${cls.id}" data-kind="${kind}" data-path="${escapeHtml(relPath)}" ` +
    (anchor === undefined ? "" : `data-anchor="${anchor}" `) +
    `data-name="${escapeHtml(name)}">${label}</button>`
  );
}

/**
 * SPEC §9 — an `HH:MM` beside a transcript's citation becomes a link that
 * opens the transcript at that heading, through the `## HH:MM` ids the
 * document register gives a transcript's headings.
 *
 * "Beside" is the section: a time takes the transcript cited before it, or —
 * since the model writes the time first as readily as last, "he said it at
 * 01:23 in `<path>`" — the first one cited anywhere in the section, until
 * some other document is cited, which ends the carry so a time beside a guide
 * does not link into a transcript named further up. A section that cited no
 * transcript leaves its times as text, a clock time keeps its meridiem and
 * stays text wherever it appears, a time inside a code block is code, and a
 * heading ends the carry.
 *
 * A time linked to a heading the transcript happens not to have opens it at
 * the top, which is what the viewer does with any anchor it cannot find — so
 * the cost of reaching one bullet too far is a scroll, not a wrong document.
 */
function linkTimes(html: string): string {
  return html
    .split(SECTION_END)
    .map((section) => {
      const cites = section.match(new RegExp(CITE.source, "g")) ?? [];
      const fallback = cites.map(citedTranscript).find((c) => c !== null) ?? null;
      if (fallback === null) return section;
      // `null` until the first citation; from then on whatever was cited
      // last, so a non-transcript citation suppresses the link rather than
      // falling back past it.
      let current: Cited | null | undefined = undefined;
      return section.replace(
        PRE_CITE_TAG_OR_TIME,
        (
          whole: string,
          _pre: string | undefined,
          button: string | undefined,
          _path: string | undefined,
          _tag: string | undefined,
          time: string | undefined,
          _hour: string | undefined,
          _minute: string | undefined,
          at: number,
        ) => {
          if (button !== undefined) {
            current = citedTranscript(button);
            return whole;
          }
          if (time === undefined) return whole;
          if (CLOCK_TIME.test(section.slice(at + time.length))) return whole;
          const cite = current === undefined ? fallback : current;
          if (cite === null) return whole;
          return citeButton(
            { id: cite.classId, color: cite.color },
            cite.path,
            "md",
            time,
            time,
          );
        },
      );
    })
    .join("");
}

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
  const cited = html.replace(
    /(?<!<pre>)<code>([^<]+)<\/code>/g,
    (whole, inner: string) => {
      const path = decodeEntities(inner);
      const cls = classes.find((c) => path.startsWith(`${c.folderName}/`));
      if (!cls) return whole;
      const relPath = path.slice(cls.folderName.length + 1);
      const kind = VIEWABLE[relPath.split(".").pop()?.toLowerCase() ?? ""];
      if (!kind) return whole;
      return citeButton(cls, relPath, kind, inner);
    },
  );
  // Times last: they borrow the class and colour of the citation beside them,
  // so the citations have to exist first.
  return linkTimes(cited);
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
