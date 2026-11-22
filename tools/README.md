# tools/

One thing lives here: `tools/llama.cpp/` — pinned as a git submodule to upstream
`ggml-org/llama.cpp` (git commit `46eb1742db6d2caec1bfa848c5702b1bdcc815e5`,
"llama-grammar: Fix integer overflow (#32551)", 2026-11-21). Clone with
`git clone --recurse-submodules` (or run `git submodule update --init --recursive`)
to populate it.

## Why it is here

ATLAS runs the model on this machine, and the model server is llama.cpp. `bin/llama-server`
and the dylibs beside it are compiled from this tree. It is vendored so that building needs
no network, and so the exact source of the binary that ships is in the repository.

## Nothing here runs at app run time

`atlas start` launches `bin/llama-server` as a child process on `127.0.0.1`; it never reads
`tools/`. No ATLAS code compiles, loads or executes anything under `tools/` — the directory
is input to a build a person runs, and to nothing else. A built `bin/llama-server` starts the
app whether or not this source tree is still on disk.

What changed: the build used to be a four-line recipe in this file, and on a fresh machine
nothing turned the recipe into a binary — `atlas start` failed with "the model server is
missing". It is a Rust subcommand now:

    rust/target/release/atlas-setup llama      # prints the plan, changes nothing
    rust/target/release/atlas-setup llama --yes   # builds and installs it

`atlas-setup` is a separate binary: no crate links it and `atlas start` never spawns it. It
takes one subcommand, from a person, and without `--yes` it prints the URL or the argv and
exits 3. `docs/OPERATIONS.md` has the whole provisioning story; `atlas-setup check` answers
"is this machine ready" in seven lines.

## What the build does, exactly

    cmake -S tools/llama.cpp -B tools/llama.cpp/build -DCMAKE_BUILD_TYPE=Release
    cmake --build tools/llama.cpp/build --target llama-server -j <cores>
    # then: build/bin/llama-server and every shared library beside it -> bin/

The same thing as one command is `bash rust/build.sh`'s sibling, the subcommand above. The
argv is one argument per element (no `sh -c`: `atlas-platform::child::command` spawns cmake
directly), the source directory keeps its `CMakeLists.txt`, and the copy is per file —
`bin/llama-server` is never a half-written binary. `bin/llama-server` is the path
`atlas-launcher` looks for first, falling back to a `llama-server` on `PATH`.

`rm -rf tools/llama.cpp/build bin/llama-server` and rebuilding are safe: nothing in `data/`
or `models/` is touched.

## What is in the tree that is not ours

It is upstream, unmodified, so it carries its own tooling: 268 Python files
(`convert_hf_to_gguf.py`, `gguf-py/`, tests). Those are upstream's utilities for *making*
GGUF model files. ATLAS never runs them and `bin/` ships no Python. They are the reason the
language scans in `docs/RUST-PROOF.md` exclude `tools/llama.cpp/`.

## The Python suite that used to be documented here is gone

This file used to index `tools/*.py`, the Python check suite. Those files, and the `server/`
package they tested, were deleted when the app became Rust. The checks are cargo's now:

    cargo test --offline --workspace      # unit tests, every crate
    cargo run -p atlas-e2e -- run         # 31 steps: HTTP routes, boot audit, real browser

`docs/RUST-PROOF.md` §5 has the audit that proves no Python is left in the app.
