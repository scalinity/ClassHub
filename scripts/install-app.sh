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

cd "$REPO"
COMMIT="$(git rev-parse --short HEAD)"
if [[ -n "$(git status --porcelain)" ]]; then
  echo "install-app: the working tree has uncommitted changes — the bundle will carry them under $COMMIT" >&2
fi

# Quit the installed instance only, never a dev build: both carry the same
# bundle identifier, and the dev build may be mid-job. Matching on the
# executable path picks the right process, and addressing the Apple Event by
# bundle path delivers the quit to that one. Quit rather than kill, so the job
# runner's shutdown still reaps any `claude` it spawned.
if pgrep -f "^$EXE" >/dev/null; then
  echo "install-app: quitting the installed ClassHub (pid $(pgrep -f "^$EXE" | tr '\n' ' '))"
  osascript -e "tell application \"$APP\" to quit" >/dev/null
  for _ in {1..60}; do
    pgrep -f "^$EXE" >/dev/null || break
    sleep 0.5
  done
  if pgrep -f "^$EXE" >/dev/null; then
    echo "install-app: ClassHub did not quit within 30s — quit it yourself and run this again" >&2
    exit 1
  fi
fi

echo "install-app: building $COMMIT"
npm run tauri build -- --bundles app

# Replace rather than merge: ditto onto an existing bundle keeps files the new
# build no longer ships. The build has already succeeded by this line, so a
# failed build never removes the working app.
rm -rf "$APP"
ditto "$BUILT" "$APP"
echo "install-app: $COMMIT is installed at $APP"
open "$APP"
