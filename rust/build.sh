#!/usr/bin/env bash
# Build ATLAS from source. One command, one toolchain.
#
#   bash rust/build.sh
#
# This used to build the "optional Rust cores" whose Python fallbacks took over
# when they were absent. There are no fallbacks any more: this workspace is the
# product, and `cargo build --release` builds every binary the app runs. The face of
# that product is `rust/target/release/atlas-desktop` -- a native window, no browser.
#
# Nothing here downloads at run time. The first build on a fresh machine fetches
# crates from the network — that is a BUILD step and it says so; every later build
# works with `--offline` from the cargo cache.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
cd "$ROOT" || exit 1

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust toolchain not found (cargo)."
  echo "Install it once by hand from https://rustup.rs — nothing here installs or downloads it —"
  echo "open a new terminal, then run: bash rust/build.sh"
  # Exit 1, not 0: a build that produced nothing must not read as success to `&&`, to a
  # wrapper script, or to the person reading the last line.
  exit 1
fi
echo "rustc $(rustc --version | awk '{print $2}')"

echo "Building the workspace…"
# `--locked` on both attempts, and it is not decoration: without it, `cargo build`
# is allowed to *re-resolve* the dependency graph, so a manifest edit (or a stale
# lock) silently produces binaries from versions nobody chose, while the lockfile
# in the tree still describes the old set. With it, a manifest that does not match
# `Cargo.lock` is a hard error instead of a supply-chain change. The first attempt
# is `--offline`; the retry exists only to fill an empty crate cache on a fresh
# machine, and it still may not change a single version.
if ! cargo build --release --offline --locked; then
  echo "The offline build failed: on a fresh machine the crate cache is empty."
  echo "Retrying once with the network — dependencies are fetched at BUILD time only."
  if ! cargo build --release --locked; then
    echo "Build failed. Run rust/target/release/atlas doctor for the missing piece."
    exit 1
  fi
fi

# The binaries the app runs, and nothing else: a missing one is a sentence. The
# first is the product's face (the egui window); the rest are the engines it and
# `atlas` spawn. The engine list is `atlas_web::native::REQUIRED` — one list,
# checked in two places, so they cannot disagree.
missing=0
for binary in atlas-desktop atlas atlas-guard atlas-core atlas-text atlas-net atlas-store \
              atlas-places atlas-graph atlas-topics atlas-dates; do
  if [ ! -x "$ROOT/target/release/$binary" ]; then
    printf 'missing after build: rust/target/release/%s\n' "$binary"
    missing=1
  fi
done
[ "$missing" -eq 0 ] || exit 1
echo "Built: rust/target/release (atlas-desktop, atlas + the nine engines it spawns)"

# The policy gate: no fetch primitive may reach shipped code, and no listener may
# be pinned anywhere but loopback. `atlas-config audit` is the Rust implementation
# of that check — it is read-only and starts nothing.
# `< /dev/null` is not decoration: the engine reads stdin, and a script that inherits a
# terminal would wait for an EOF that never comes. (Measured before both fixes:
# `bash rust/build.sh` hung for 15 minutes at exactly this line. The engine now also
# treats a terminal as "no input", so the two defences are independent.)
if [ -x "$ROOT/target/release/atlas-config" ]; then
  if audit=$("$ROOT/target/release/atlas-config" audit 2>&1 < /dev/null); then
    printf 'policy   ok    no download primitive, no unpinned listener\n'
  else
    printf 'policy   FAIL  %s\n' "$audit"
    exit 1
  fi
else
  printf 'policy   FAIL  atlas-config is missing, so the gate cannot run\n'
  exit 1
fi

# The web bundle is not built here, and nothing in this script reads it. `public/ui/`
# is the retired frontend's page: the face is now `atlas-desktop`, a native window
# that reads `data/atlas.db` and opens no socket. `rust/build-ui.sh`, which made that
# bundle, is not run by this script: no step of this build depends on its output.
printf 'web UI  %s\n' "not built (retired; the face is rust/target/release/atlas-desktop)"

exit 0
