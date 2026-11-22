#!/usr/bin/env bash
# ATLAS run.sh — open the native window (macOS / Linux).
#
#   ./run.sh            build it if it is missing, then open the window
#   ./run.sh --help     this text, and nothing else
#
# The product is one Rust binary: rust/target/release/atlas-desktop, an egui window
# that reads data/atlas.db read-only. It opens no socket and serves no page — there is
# no port to wait for and no address to open, which is why none of that is here any
# more.
#
# It does not start the model server either. Chat is the one view that needs
# bin/llama-server, and the window's status row says whether it is there instead of
# guessing; the model server and the model file are provisioned by setup.sh, and
# `atlas start` (rust/target/release/atlas) is the one command that starts the server
# before the window. The last lines this script prints say which of the two applies.
#
# So this script only does what a shell does better than the binary: make sure that
# binary exists, then exec it. Nothing is downloaded or installed at run time; a
# missing toolchain is a sentence with the exact command, not a crash.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

# Everything this script creates inherits 0700/0600: no log or database written from
# here is ever readable by another account on the machine.
umask 077
mkdir -p data
chmod 700 data 2>/dev/null || true

say() { printf '[run] %s\n' "$*"; }
die() { printf '[run] %s\n' "$*" >&2; exit 1; }

DESKTOP="$ROOT/rust/target/release/atlas-desktop"

# The window parses no arguments: it finds the project from its own path and reads
# data/atlas.db. Forwarding an argument it would ignore is a promise this script
# cannot keep, so an argument is refused with the sentence that says so.
case "${1:-}" in
  "") ;;
  -h|--help)
    printf '%s\n' \
      'ATLAS run.sh — open the native window' \
      '' \
      '  ./run.sh          build rust/target/release/atlas-desktop if it is missing, then open it' \
      '  ./run.sh --help   this text' \
      '' \
      'The window takes no arguments. It reads data/atlas.db, opens no socket and serves' \
      'no page: the web frontend and the port it served are retired.'
    exit 0
    ;;
  *)
    die "the native window takes no arguments (it is not \`atlas start\` any more): ./run.sh" ;;
esac

if [ ! -x "$DESKTOP" ]; then
  command -v cargo >/dev/null 2>&1 \
    || die "Rust is missing. Install it once from https://rustup.rs, then run ./setup.sh"
  say "the window is not built: cargo build --release --offline --locked -p atlas-desktop"
  # `--locked` on both attempts: the retry may fill an empty crate cache, it may never
  # re-resolve (rust/build.sh has the long version of why).
  if ! (cd rust && cargo build --release --offline --locked -p atlas-desktop); then
    say "the offline build failed — on a fresh machine the crate cache is empty."
    say "retrying once with the network: crates are fetched at BUILD time only."
    (cd rust && cargo build --release --locked -p atlas-desktop) \
      || die "cargo build failed — run ./setup.sh for the full report"
  fi
  [ -x "$DESKTOP" ] || die "rust/target/release/atlas-desktop is still missing — run ./setup.sh"
fi

# The one file the window reads that cargo does not produce. Its absence is not a
# refusal — the window opens, shows an empty archive and says so in its status row --
# so this is a line, not a stop.
if [ ! -f "$ROOT/data/atlas.db" ]; then
  say "data/atlas.db is not there yet: the window opens with an empty archive and says so."
fi

# Chat is the one view that dials the model server, and this script starts nothing but
# the window: the two pieces chat needs are named here rather than started here, and the
# rest of the window works without them. No port is probed — the window itself reports
# whether the server answers, and a probe would open the socket this script promises not
# to open.
if [ -f "$ROOT/bin/llama-server" ] && ls "$ROOT"/models/*.gguf >/dev/null 2>&1; then
  say "chat needs the model server running: rust/target/release/atlas start starts it ahead of the window."
else
  say "chat will report a missing model server: bin/llama-server or the model file under models/ is not there."
  say "./setup.sh --provision builds the first and fetches the second; the window opens without them."
fi

exec "$DESKTOP"
