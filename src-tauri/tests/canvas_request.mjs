// Runs REQUEST_JS from canvas.rs against a mock Canvas page.
//
// The script is the whole Canvas transport: it starts a fetch inside the page,
// parks the result on `window`, and hands it back to a later evaluation. None
// of that can be executed from Rust, so the Rust tests can only assert that
// certain text appears in the source — which passes just as happily when the
// branch is inverted. This drives the actual behaviour.
//
// Driven by `canvas::tests::the_request_script_behaves_against_a_mock_page`,
// which skips when node is unavailable.
//
// Usage: node tests/canvas_request.mjs src/canvas.rs

import { readFileSync } from "node:fs";

// Pull the raw string straight out of canvas.rs, so the harness can never drift
// from the script the app actually injects.
const src = readFileSync(process.argv[2], "utf8");
const OPEN = 'const REQUEST_JS: &str = r#"';
const start = src.indexOf(OPEN);
if (start === -1) {
  console.log("FAIL could not find the REQUEST_JS raw string in", process.argv[2]);
  process.exit(1);
}
const REQUEST_JS = src.slice(start + OPEN.length).split('"#;')[0];

const HOST = "ufl.instructure.com";
const MAX = 48 * 1024 * 1024;

function build({ binary = false, max = MAX, id = "q1", path = "/api/v1/x" } = {}) {
  // Same order canvas.rs substitutes in, so a placeholder inside a value cannot
  // be rewritten by a later pass.
  return REQUEST_JS.replace("__BINARY__", binary ? "true" : "false")
    .replace("__MAXBYTES__", String(max))
    .replace("__ID__", JSON.stringify(id))
    .replace("__HOST__", JSON.stringify(HOST))
    .replace("__PATH__", JSON.stringify(path));
}

// `window` persists across polls the way the page's own globals do, which is
// what makes the park-and-collect protocol testable at all.
function evaluate(script, env) {
  const fn = new Function("window", "location", "fetch", "btoa", `return (${script});`);
  return fn(env.window, env.location, env.fetch, globalThis.btoa);
}

function response({ status = 200, body = "", bytes = null, headers = {} } = {}) {
  const lower = {};
  for (const [k, v] of Object.entries(headers)) lower[k.toLowerCase()] = String(v);
  return {
    status,
    ok: status >= 200 && status < 300,
    headers: { get: (name) => lower[name.toLowerCase()] ?? null },
    text: async () => body,
    arrayBuffer: async () => {
      if (bytes === null) throw new Error("arrayBuffer called when it should not have been");
      return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    },
  };
}

const flush = () => new Promise((resolve) => setImmediate(resolve));

let failures = 0;
function check(name, condition, detail) {
  if (condition) return;
  failures += 1;
  console.log(`FAIL ${name}${detail === undefined ? "" : ` — ${detail}`}`);
}

// --- the host check comes before the fetch -----------------------------------
{
  let called = false;
  const env = {
    window: {},
    location: { host: "login.ufl.edu" },
    fetch: () => {
      called = true;
      return Promise.resolve(response());
    },
  };
  const out = evaluate(build(), env);
  check("offsite reports where it is", out.state === "offsite", JSON.stringify(out));
  check("offsite host is named", out.host === "login.ufl.edu");
  check("offsite never fetches", called === false);
}

// --- park, then collect, without forgetting ----------------------------------
{
  const env = {
    window: {},
    location: { host: HOST },
    fetch: () => Promise.resolve(response({ body: '[{"id":1}]', headers: { Link: "<x>; rel=\"n\"" } })),
  };
  const script = build();
  const first = evaluate(script, env);
  check("the first evaluation is pending", first.state === "pending", JSON.stringify(first));

  const second = evaluate(script, env);
  check("a second evaluation while in flight stays pending", second.state === "pending");

  await flush();
  const third = evaluate(script, env);
  check("the result comes back", third.state === "response", JSON.stringify(third));
  check("the body is carried whole", third.body === '[{"id":1}]');

  // The slot must survive being read. Rust clears it once the value has
  // actually been marshalled out; forgetting it here means an eval that times
  // out mid-flight loses the answer AND the record that anything was started,
  // and the next poll refetches a file that had already arrived.
  const fourth = evaluate(script, env);
  check("the result is not forgotten when read", fourth.state === "response", JSON.stringify(fourth));
}

// --- a refusal is text in both modes -----------------------------------------
{
  const env = {
    window: {},
    location: { host: HOST },
    fetch: () => Promise.resolve(response({ status: 403, body: "<!DOCTYPE html><html>denied" })),
  };
  const script = build({ binary: true });
  evaluate(script, env);
  await flush();
  const out = evaluate(script, env);
  check("a refused download is read as text", out.state === "response" && out.body !== undefined, JSON.stringify(out));
  check("a refused download carries no bytes", out.b64 === undefined);
  check("the status survives", out.status === 403);
}

// --- a binary success encodes every byte -------------------------------------
{
  // Deliberately across the 0x8000 chunk boundary: String.fromCharCode.apply
  // overflows its argument limit on anything megabyte-sized, so the script
  // chunks, and an off-by-one there would corrupt the file silently.
  const size = 0x8000 * 2 + 5;
  const bytes = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) bytes[i] = (i * 7 + 13) % 256;
  const env = {
    window: {},
    location: { host: HOST },
    fetch: () => Promise.resolve(response({ bytes, headers: { "Content-Length": size } })),
  };
  const script = build({ binary: true });
  evaluate(script, env);
  await flush();
  await flush();
  const out = evaluate(script, env);
  check("a binary success comes back as bytes", out.state === "response" && !!out.b64, JSON.stringify(out));
  check(
    "every byte survives the chunking",
    out.b64 === Buffer.from(bytes).toString("base64"),
    `${(out.b64 || "").length} chars for ${size} bytes`,
  );
}

// --- the ceiling is refused before the body is read --------------------------
{
  const env = {
    window: {},
    location: { host: HOST },
    // bytes: null makes arrayBuffer throw, so reaching it fails the test.
    fetch: () => Promise.resolve(response({ bytes: null, headers: { "Content-Length": MAX + 1 } })),
  };
  const script = build({ binary: true });
  evaluate(script, env);
  await flush();
  const out = evaluate(script, env);
  check("an oversize file is refused", out.state === "toolarge", JSON.stringify(out));
  check("the size is reported", out.bytes === MAX + 1);
}

// --- a file with no Content-Length is still bounded --------------------------
{
  const bytes = new Uint8Array(64);
  const env = {
    window: {},
    location: { host: HOST },
    fetch: () => Promise.resolve(response({ bytes })),
  };
  const script = build({ binary: true, max: 32 });
  evaluate(script, env);
  await flush();
  await flush();
  const out = evaluate(script, env);
  check("the bytes themselves are checked too", out.state === "toolarge", JSON.stringify(out));
}

// --- a fetch that never completes ---------------------------------------------
{
  const env = {
    window: {},
    location: { host: HOST },
    fetch: () => Promise.reject(new Error("Load failed")),
  };
  const script = build();
  evaluate(script, env);
  await flush();
  const out = evaluate(script, env);
  check("a failed fetch reports an error", out.state === "error", JSON.stringify(out));
  check("the reason is carried", out.message === "Load failed");
}

if (failures > 0) {
  console.log(`${failures} check(s) failed`);
  process.exit(1);
}
console.log("OK canvas request script");
