import { marked } from "marked";

import { escapeHtml } from "@/lib/answer";

/**
 * A document-level CSP for HTML the app did not write: generated study guides
 * and class notebooks. Both need their own inline scripts and styles to work,
 * so the sandbox attribute alone has to keep `allow-scripts` — this closes what
 * the sandbox does not, which is egress. `default-src 'none'` covers fetch,
 * XHR, WebSocket and beacon by fallback, so a prompt-injected guide cannot
 * phone home with what it read. SPEC §8.1 states the same rule as prose in the
 * prompt; this is the half that holds when the prose is ignored.
 */
const DOC_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:";

const CSP_META = `<meta http-equiv="Content-Security-Policy" content="${DOC_CSP}">`;

/**
 * Injects the policy as early in the document as possible — a CSP meta only
 * governs what follows it. Multiple policies intersect rather than override, so
 * a document that already carries one keeps whichever rule is stricter.
 */
export function withDocumentCsp(html: string): string {
  const head = /<head[^>]*>/i.exec(html);
  if (head) {
    const at = head.index + head[0].length;
    return html.slice(0, at) + CSP_META + html.slice(at);
  }
  const openHtml = /<html[^>]*>/i.exec(html);
  if (openHtml) {
    const at = openHtml.index + openHtml[0].length;
    return `${html.slice(0, at)}<head>${CSP_META}</head>${html.slice(at)}`;
  }
  return CSP_META + html;
}

/** Front matter renders as a quiet mono block, the body through marked. */
export function renderMarkdown(src: string): string {
  let front = "";
  let body = src;
  if (src.startsWith("---\n")) {
    const end = src.indexOf("\n---\n", 4);
    if (end !== -1) {
      front = `<pre class="frontmatter">${escapeHtml(src.slice(4, end))}</pre>`;
      body = src.slice(end + 5);
    }
  }
  return front + (marked.parse(body, { async: false }) as string);
}

/**
 * ClassHub's document register for raw materials: same paper/ink/type system
 * as the generated guides, but neutral apparatus — raw sources are unbranded;
 * only synthesized documents carry the class accent. The note editor's live
 * preview uses it too, so a note previews exactly as it will read.
 */
export function docShell(body: string): string {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
<!-- The sandbox already blocks scripts; the CSP closes the residual it
     doesn't cover — network loads (remote images, meta refresh) from note
     or material content rendered through this shell. -->

<style>
:root {
  --paper: #fcfcfd; --ink: #1c1f24; --muted: #697079;
  --hairline: #e3e5e9; --shade: rgba(28, 31, 36, 0.05);
}
@media (prefers-color-scheme: dark) {
  :root {
    --paper: #191b1f; --ink: #e6e8eb; --muted: #8f959d;
    --hairline: rgba(255,255,255,0.12); --shade: rgba(255,255,255,0.05);
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--paper); color: var(--ink);
  font: 15px/1.6 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
}
main { max-width: 72ch; margin: 0 auto; padding: 3rem 2rem 5rem; overflow-wrap: break-word; }
h1, h2, h3, h4 { font-family: ui-serif, "New York", Georgia, "Times New Roman", serif; line-height: 1.25; font-weight: 600; }
h1 { font-size: 28px; margin: 0 0 0.6em; }
h2 { font-size: 21px; margin: 2em 0 0.5em; padding-top: 1em; border-top: 1px solid var(--hairline); }
h3 { font-size: 17px; margin: 1.6em 0 0.4em; }
pre, code { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
pre {
  font-size: 13px; line-height: 1.55; background: var(--shade);
  border: 1px solid var(--hairline); border-radius: 8px;
  padding: 0.8rem 0.95rem; overflow-x: auto;
}
code { font-size: 13px; }
p code, li code { background: var(--shade); border-radius: 4px; padding: 0.1em 0.35em; font-size: 12.5px; }
pre code { background: none; padding: 0; }
pre.sheet, pre.frontmatter { white-space: pre-wrap; }
pre.frontmatter { font-size: 11px; color: var(--muted); margin-bottom: 2rem; }
blockquote { margin: 1em 0; padding-left: 1em; border-left: 2px solid var(--hairline); color: var(--muted); }
table { border-collapse: collapse; margin: 1em 0; }
th, td { border: 1px solid var(--hairline); padding: 0.45em 0.8em; text-align: left; }
th { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid var(--hairline); margin: 2em 0; }
a { color: inherit; }
@media print { :root { --paper: #fff; --ink: #000; } }
</style>
</head>
<body><main>${body}</main></body>
</html>`;
}
