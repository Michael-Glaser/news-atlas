#!/usr/bin/env bash
# ATLAS setup.sh — provision a fresh machine, then check it (macOS / Linux).
#
#   ./setup.sh              build, report what is missing, do not fetch anything big
#   ./setup.sh --provision  build, then build the model server and fetch the model
#
# Five steps, and the interesting ones are 2, 3 and 4:
#
#   1. Rust         the toolchain must be there; a missing one is a sentence
#   2. build        cargo build --release --offline --locked (once with the network if
#                   the crate cache is empty — a BUILD step, never a run-time fetch),
#                   then the check that the product's face was produced:
#                   rust/target/release/atlas-desktop, the native window
#   3. atlas-setup  the pieces cargo does not build: the vendored llama.cpp tree,
#                   bin/llama-server, the pinned model file. One line each, each naming
#                   the subcommand that fixes it. They are for chat: the window starts
#                   without them. --provision runs them; without it nothing large is
#                   fetched and nothing is built. The one download is the 6.5 GB model,
#                   from the URL printed first, over https, verified against a pinned
#                   sha256 before use.
#   4. the window   the two files the window actually opens: data/atlas.db and the
#                   vendored map outlines. Read-only in both directions — this step
#                   binds no port and probes none.
#   5. data privacy 0700 on data/, 0600 on the files this script created.
#
# The app itself never fetches: `atlas-setup` is a separate binary that no crate links,
# and the window never spawns it. See docs/OPERATIONS.md.
# Idempotent: run it again after you provisioned something.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

DESKTOP="$ROOT/rust/target/release/atlas-desktop"
SETUP="$ROOT/rust/target/release/atlas-setup"
log() { printf '[setup] %s\n' "$*"; }
warn() { printf '[setup] %s\n' "$*"; }
die() { printf '[setup] %s\n' "$*" >&2; exit 1; }

# The arguments are read before anything is created: `./setup.sh --help` is a person
# asking what the script does, and it must not make a directory, change a mode or start
# a build on the way to answering. It used to `mkdir -p data` first, so a help request
# wrote to the repository.
PROVISION=0
for arg in "$@"; do
  case "$arg" in
    --provision) PROVISION=1 ;;
    -h|--help)
      # The header comment *is* the help, delimited by the first statement rather than
      # by a line number: a range like `2,32p` silently cuts the help mid-sentence the
      # first time a line is added above it.
      sed -n '2,/^set /p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "unknown argument $arg (only --provision and --help)" ;;
  esac
done

umask 077
mkdir -p data
chmod 700 data 2>/dev/null || true

# ---------- 1. Rust ----------------------------------------------------------
command -v cargo >/dev/null 2>&1 \
  || die "Rust is missing. Install it once from https://rustup.rs, open a new terminal, then run ./setup.sh again."
log "$(cargo --version) / $(rustc --version)"

# ---------- 2. build ---------------------------------------------------------
# Both attempts are `--locked` (see rust/build.sh): the retry may fill an empty
# crate cache, it may never pick a version `Cargo.lock` does not already name.
log "cargo build --release --offline --locked"
if ! (cd rust && cargo build --release --offline --locked); then
  warn "the offline build failed — on a fresh machine the crate cache is empty."
  warn "retrying once with the network: dependencies are fetched at BUILD time,"
  warn "never at run time. Nothing else in ATLAS reaches the network."
  (cd rust && cargo build --release --locked) || die "cargo build failed — ATLAS cannot run without its workspace."
fi
[ -x "$DESKTOP" ] || die "rust/target/release/atlas-desktop was not produced — the workspace is not what this script expects."
[ -x "$SETUP" ] || die "rust/target/release/atlas-setup was not produced — the workspace is not what this script expects."
log "built: rust/target/release (atlas-desktop, the engines and the tools)"

# ---------- 3. atlas-setup: the pieces cargo does not build -------------------
# `atlas-setup check` is the question; `atlas-setup all --yes` is the action. The
# check runs first either way, so the run below never starts on a guess. Its report
# is its own: the lines above are what it checks, not what this script claims.
if "$SETUP" check --root "$ROOT"; then
  log "atlas-setup: every piece it checks is present."
elif [ "$PROVISION" = 1 ]; then
  log "--provision: running atlas-setup all --yes — it prints each step it takes"
  "$SETUP" all --root "$ROOT" --yes \
    || die "atlas-setup failed — the line above names the piece and the reason. Fix it and re-run ./setup.sh --provision."
else
  warn "the pieces above are missing. Nothing was fetched or built."
  warn "chat needs bin/llama-server and the pinned model; the window itself needs neither."
  warn "run ./setup.sh --provision, or run the subcommand each line named (atlas-setup is in rust/target/release)."
fi

# ---------- 4. the window's own files ---------------------------------------
# No `atlas doctor` here any more: the launcher's doctor probes the model server's
# loopback port before every boot (the probe connects; it never binds), and this build
# opens no port at all, so a setup step has no business probing one. What the window
# opens is two files, so those are what this step reads — and it reads them, it does not
# touch them.
if [ -f "$ROOT/data/atlas.db" ]; then
  log "archive: data/atlas.db ($(du -h "$ROOT/data/atlas.db" 2>/dev/null | cut -f1)) — the window reads it read-only"
else
  warn "archive: data/atlas.db is not there yet — the window opens with an empty archive and says so in its status row."
  warn "it is written by the ingest pipeline, never by the window."
fi
if [ -f "$ROOT/public/vendor/ne_50m_countries.geojson" ]; then
  log "map outlines: public/vendor/ne_50m_countries.geojson"
else
  warn "map outlines: public/vendor/ne_50m_countries.geojson is missing — the map view draws the graticule and the pins, no coastlines."
fi

# ---------- 5. data privacy -------------------------------------------------
chmod 600 data/*.json data/*.log data/atlas.db 2>/dev/null || true
log "data/: directory 0700, files 0600 (this account only)"

log "ready. open the window with ./run.sh (or: rust/target/release/atlas-desktop)"
