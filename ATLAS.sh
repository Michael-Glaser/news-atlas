#!/bin/bash
# ATLAS - double-click this file on Linux to open the app.
#
# What it does, in order, and nothing else:
#
#   1. checks that this machine has the developer tools ATLAS is built with (once);
#   2. builds ATLAS if this copy has never been built - a build step, on this machine;
#   3. runs `atlas setup --install`, which fetches the model file (the one download, and
#      it can be paused and resumed) and builds the model server;
#   4. opens the app.
#
# It never downloads anything itself: the only thing here that fetches bytes is
# `atlas-setup --install`, and only after it has printed the address it will use. It
# opens no port. If a step cannot be done, it stops with one sentence and waits for a
# key press so a terminal started by a file manager does not vanish.
set -uo pipefail

cd "$(dirname "$0")" || exit 1
ROOT="$PWD"
RELEASE="$ROOT/rust/target/release"

say() { printf '%s\n' "$*"; }
blank() { printf '\n'; }

stop() {
  blank
  printf 'ATLAS could not start: %s\n' "$*" >&2
  blank
  printf 'Press return to close this window.\n'
  # A window started by a double-click may have no keyboard attached to it (some file
  # managers run the script without a terminal): waiting for a key would then return at
  # once and the sentence above would vanish. So: wait for the key if there is one, and
  # otherwise leave the text on screen long enough to photograph with a phone.
  read -r _ || sleep 20
  exit 1
}

blank
say "ATLAS - getting this machine ready."

# A folder copied from a disk image, a network share or somebody else's account is
# read-only here, and every later step would fail with a message from the operating
# system instead of from ATLAS. One sentence now is worth an hour later.
if ! ( : > "$ROOT/.atlas-write-test" ) 2>/dev/null; then
  stop "ATLAS cannot write into this folder ($ROOT). Move the whole ATLAS folder into your own home folder, for example your Documents folder, and start this file again."
fi
rm -f "$ROOT/.atlas-write-test"
blank

# --- 1. the developer tools ---------------------------------------------------------
#
# A build needs a C compiler and cmake; the model server is compiled from the copy of
# llama.cpp that ships with ATLAS. Both are named here because both are one command on
# every distribution, and a build without them fails with an error nobody can act on.
if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
  say "One thing is missing: a C compiler."
  say "On Debian or Ubuntu: sudo apt install build-essential"
  say "On Fedora: sudo dnf install gcc gcc-c++ make"
  stop "the C compiler is not installed yet."
fi

if ! command -v cargo >/dev/null 2>&1; then
  say "One thing is missing: the Rust tools ATLAS is built with."
  say "Go to https://rustup.rs, follow the single step there,"
  say "then run this file again."
  stop "the Rust tools are not installed yet."
fi

# --- 2. build, if this copy has never been built ------------------------------------
if [ ! -x "$RELEASE/atlas-desktop" ] || [ ! -x "$RELEASE/atlas-setup" ] || [ ! -x "$RELEASE/atlas" ]; then
  say "Building ATLAS on this machine. This happens once and takes a few minutes."
  if ! (cd "$ROOT/rust" && cargo build --release --offline --locked); then
    say "First attempt could not finish: this machine has no copy of the building blocks yet."
    say "Trying once more, downloading them this one time."
    (cd "$ROOT/rust" && cargo build --release --locked) \
      || stop "ATLAS could not be built. The lines above are the reasons. The usual causes are no internet connection while the building blocks are fetched, and a copy that lives in a read-only folder. Connect to the internet and start this file again; if it still stops here, run ./setup.sh in a terminal and send the last ten lines."
  fi
  say "Built."
  blank
fi

# --- 3. the licence, the model, the model server ------------------------------------
#
# This is `atlas-setup --install`, and it starts with the licence: what the Apache-2.0
# terms cover, what they do not, and one question. Only a yes allows the one download -
# about 6.5 GB into models/, from huggingface.co - and the model server built from the
# copy of llama.cpp that ships beside this file. Any other answer stops here, with
# nothing fetched and nothing written.
say "Next: the licence question, then one download of about 6.5 GB into models/."
say "If the connection drops, start this file again - it carries on from where it stopped."
if [ ! -x "$RELEASE/atlas-setup" ]; then
  stop "the setup program is missing from rust/target/release. Run ./setup.sh in a terminal."
fi
"$RELEASE/atlas-setup" --install || stop "the setup step above says what is missing."

# --- 4. the app ---------------------------------------------------------------------
blank
say "Opening ATLAS. The first time, it shows the licence in full: tick the line under it"
say "and the workspace opens. Close its window to stop ATLAS."
blank
"$RELEASE/atlas" start || stop "the app closed with an error; the lines above say why."
