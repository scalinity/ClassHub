#!/bin/zsh
# The one place a production build is run (SPEC §13). Quits the installed app,
# builds the bundle from the working tree, replaces /Applications/ClassHub.app
# with it, and relaunches — so the app the semester runs on is the commit the
# session just accepted, and the installed build and a dev build stop drifting
# in code the way they once drifted in data. Run as `npm run install-app`.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP=/Applications/ClassHub.app
EXE="$APP/Contents/MacOS/classhub"
BUILT="$REPO/src-tauri/target/release/bundle/macos/ClassHub.app"

# The installed instance, by its exact command line. A dev build runs from
# target/debug, so it never matches. `-x` anchors the whole line and the dots
# are escaped, since `pgrep -f` reads its pattern as a regular expression.
installed_pids() {
  pgrep -f -x "${EXE//./\\.}" || true
}

cd "$REPO"
COMMIT="$(git rev-parse --short HEAD)"
if [[ -n "$(git status --porcelain)" ]]; then
  echo "install-app: the working tree has uncommitted changes — the bundle will carry them under $COMMIT" >&2
fi

# Quit the installed instance only, never a dev build. Both carry the same
# bundle identifier, so an Apple Event addressed by name, id or path can land
# on either; NSRunningApplication looked up by pid cannot. `terminate` is the
# Dock's graceful quit, so the job runner's shutdown still reaps any `claude`
# it spawned, where a signal would skip that. In JXA a zero-argument method is
# a property access, so `app.terminate` without parentheses is the call.
pids="$(installed_pids)"
if [[ -n "$pids" ]]; then
  echo "install-app: quitting the installed ClassHub (pid ${pids//$'\n'/ })"
  for pid in ${(f)pids}; do
    osascript -l JavaScript -e 'ObjC.import("AppKit");
      const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier('"$pid"');
      if (!app.isNil()) { app.terminate; }' >/dev/null
  done
  for _ in {1..60}; do
    [[ -z "$(installed_pids)" ]] && break
    sleep 0.5
  done
  if [[ -n "$(installed_pids)" ]]; then
    echo "install-app: ClassHub did not quit within 30s — quit it yourself and run this again" >&2
    exit 1
  fi
fi

echo "install-app: building $COMMIT"
npm run tauri build -- --bundles app

# A build exiting 0 does not prove the bundle is where this expects it — a
# renamed product or a different target directory moves it — so nothing is
# removed until the replacement is confirmed to exist.
if [[ ! -d "$BUILT" ]]; then
  echo "install-app: the build produced no bundle at $BUILT — $APP left in place" >&2
  exit 1
fi
# Replace rather than merge: ditto onto an existing bundle keeps files the new
# build no longer ships.
rm -rf "$APP"
ditto "$BUILT" "$APP"
echo "install-app: $COMMIT is installed at $APP"
open "$APP"
