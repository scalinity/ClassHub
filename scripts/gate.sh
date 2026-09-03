#!/bin/zsh
# Commit gate: `cargo test` run directly, so its own exit status decides — never
# piped through grep, which would hide a failure behind grep's status — then
# `tsc --noEmit` when frontend files are staged, then `git commit -F <message>`.
# A failing gate leaves the staged files staged; `git reset` and re-stage per fix.
# usage: scripts/gate.sh <message-file>
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="$ROOT/src-tauri/target/gate-test.log"
mkdir -p "$ROOT/src-tauri/target"
(cd "$ROOT/src-tauri" && cargo test > "$LOG" 2>&1)
rc=$?
if [ $rc -ne 0 ]; then
  echo "GATE: cargo test failed (exit $rc)"; grep -E 'FAILED|panicked|^error' "$LOG" | head -20; exit 1
fi
grep -E '^test result' "$LOG" | head -1
if grep -qE '^warning' "$LOG"; then echo "GATE: warnings present"; grep -E '^warning' "$LOG" | head -5; fi
if git -C "$ROOT" diff --cached --name-only | grep -qE '^src/|^index\.html$|\.tsx?$'; then
  (cd "$ROOT" && npx tsc --noEmit) || { echo "GATE: tsc failed"; exit 1; }
  echo "tsc clean"
fi
git -C "$ROOT" commit -q -F "$1" && git -C "$ROOT" log --oneline -1
