#!/bin/zsh
# Cargo runner for dev builds (wired in .cargo/config.toml): replace the
# linker's ad-hoc signature with the stable Apple Development identity, then
# run the binary. An ad-hoc signature IS the binary's content hash — a new
# code identity every build — so a Keychain grant could never survive a
# rebuild. The Keychain matches signed apps by their signing identity, which
# this keeps constant. If signing fails, the ad-hoc build still runs; the
# Keychain prompts just come back until signing works again.
BIN="$1"
shift
/usr/bin/codesign --force \
  --sign "Apple Development: Daniel Escalante (NLH85S2C4F)" \
  --identifier com.danny.classhub "$BIN" 2>/dev/null \
  || echo "dev-sign: codesign failed — running the ad-hoc build (Keychain prompts will return)" >&2
exec "$BIN" "$@"
