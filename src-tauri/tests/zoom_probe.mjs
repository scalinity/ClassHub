// Runs the PROBE from zoom.rs against a mock Zoom player, which is the only way
// this script is ever exercised outside a real signed-in recording.
//
// The probe reads an undocumented Vuex store through eval_with_callback, so
// nothing in Rust can execute it and nothing in the app can fail loudly when it
// is wrong — a broken probe polls for ten minutes and reports that it found
// nothing. Driven by `zoom::tests::the_probe_behaves_against_a_mock_player`,
// which skips when node is unavailable.
//
// Usage: node tests/zoom_probe.mjs src/zoom.rs

import { readFileSync } from "node:fs";

// Pull the PROBE raw string straight out of zoom.rs, so the harness can never
// drift from the script the app actually injects.
const src = readFileSync(process.argv[2], "utf8");
const OPEN = 'const PROBE: &str = r#"';
const start = src.indexOf(OPEN);
if (start === -1) {
  console.log("FAIL could not find the PROBE raw string in", process.argv[2]);
  process.exit(1);
}
const body = src.slice(start + OPEN.length);
const PROBE = body.slice(0, body.indexOf('"#;'));

function makeEnv(state, { passcode = false, path = "/rec/play/x", fetchImpl } = {}) {
  const doc = {
    querySelector(sel) {
      if (sel === "#app") return state === null ? null : { __vue__: { $store: { state } } };
      if (sel.includes("password")) return passcode ? {} : null;
      return null;
    },
  };
  // `window` persists across polls, the way the page's own globals do — which
  // is what makes the fetch-once behaviour testable at all.
  return { win: {}, doc, location: { pathname: path }, fetch: fetchImpl };
}

function run(env) {
  const fn = new Function("window", "document", "location", "fetch", `return (${PROBE});`);
  return fn(env.win, env.doc, env.location, env.fetch);
}

/** Lets the page settle a resolved or rejected fetch, as it would between polls. */
const settle = () => new Promise((r) => setImmediate(r));

let failures = 0;
const check = (name, cond, detail) => {
  if (cond) console.log(`  ok   ${name}`);
  else { failures++; console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};

// A broken probe usually shows up as a missing field rather than a false one,
// so a thrown TypeError has to read as a failed expectation and not as a crash
// in the harness.
const scenario = async (name, fn) => {
  try {
    await fn();
  } catch (e) {
    failures++;
    console.log(`  FAIL ${name} threw — ${e && e.message ? e.message : e}`);
  }
};

// A timed transcriptList becomes VTT the Rust parser can read.
await scenario("A", async () => {
  const out = run(makeEnv({
    transcriptList: [
      { username: "Esra Adiyeke", ts: "00:00:01.500", endTs: "00:00:04.000", text: "The mean." },
      { username: "Esra Adiyeke", ts: null, endTs: null, text: "And the median." },
      { username: "Daniel Escalante", ts: 9, endTs: 12, text: "Is that on the exam?" },
    ],
  }));
  check("A state is ready", out.state === "ready", out.state);
  check("A starts with WEBVTT", out.text.startsWith("WEBVTT\n\n"), JSON.stringify(out.text.slice(0, 20)));
  // A row with no timing of its own lands on the previous row's end. Emitted
  // bare it would carry no timestamp line, and the parser drops those silently.
  check("A carries forward a missing timestamp",
    out.text.includes("00:00:04.000 --> 00:00:04.000\nEsra Adiyeke: And the median."),
    JSON.stringify(out.text));
  check("A converts bare seconds", out.text.includes("00:00:09.000 --> 00:00:12.000"), JSON.stringify(out.text));
});

// An untimed list keeps its turns as blank-line blocks.
await scenario("B", async () => {
  const out = run(makeEnv({
    transcriptList: [
      { username: "Esra Adiyeke", ts: null, endTs: null, text: "The mean." },
      { username: "Daniel Escalante", ts: null, endTs: null, text: "Is that on the exam?" },
    ],
  }));
  check("B state is ready", out.state === "ready", out.state);
  check("B reports untimed", out.found.includes("untimed"), JSON.stringify(out.found));
  check("B separates turns with a blank line",
    out.text === "Esra Adiyeke: The mean.\n\nDaniel Escalante: Is that on the exam?\n",
    JSON.stringify(out.text));
});

// A failed caption fetch must not retry, and must fall through to the routes
// below it — the branch returns, so retrying made them unreachable.
await scenario("C", async () => {
  let calls = 0;
  const env = makeEnv({
    ccUrl: "https://ssrweb.zoom.us/cc.vtt",
    transcriptList: [{ username: "Esra", ts: 0, endTs: 2, text: "Fallback worked." }],
  }, { fetchImpl: () => { calls++; return Promise.reject(403); } });

  const first = run(env);
  check("C first poll fetches", first.state === "fetching" && calls === 1, `${first.state}/${calls}`);
  await settle();
  const second = run(env);
  check("C does not re-fetch", calls === 1, `fetch called ${calls} times`);
  check("C falls through to the list", second.state === "ready", second.state);
  check("C reports why", (second.found ?? []).some((f) => f.startsWith("cc-fetch-failed")), JSON.stringify(second.found));
  check("C used the fallback", String(second.text).includes("Fallback worked."), JSON.stringify(second.text));
});

// An expired session answers a caption URL with a 200 carrying a login page.
// Filed as source markdown that would look exactly like a successful capture.
await scenario("D", async () => {
  const env = makeEnv(
    { ccUrl: "https://ssrweb.zoom.us/cc.vtt", viewMp4Url: "https://z.zoom.us/rec.mp4" },
    { fetchImpl: () => Promise.resolve({ ok: true, text: () => Promise.resolve("<html>Sign in</html>") }) },
  );
  run(env);
  await settle();
  const out = run(env);
  check("D refuses a login page", out.state === "no-transcript", out.state);
  check("D offers the recording instead", out.mp4 === "https://z.zoom.us/rec.mp4", String(out.mp4));
});

// Transcript and recording both switched off is settled, not "keep waiting" —
// the caller's job there is to name the manual step.
await scenario("E", async () => {
  const out = run(makeEnv({ accessLevel: "meeting", transcriptList: [] }));
  check("E says no-transcript", out.state === "no-transcript", out.state);
  check("E offers no media", out.mp4 === null, String(out.mp4));
});

// The gates before the player loads still report themselves.
await scenario("F", async () => {
  check("F login", run(makeEnv(null, { path: "/signin/sso" })).state === "login");
  check("F passcode", run(makeEnv(null, { passcode: true })).state === "passcode");
  check("F no store", run(makeEnv(null)).found.includes("no-store"));
});

console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURES`);
process.exit(failures === 0 ? 0 : 1);
