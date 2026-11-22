# Third-party components

ATLAS itself is licensed under the Apache License 2.0 (see `LICENSE` and
`NOTICE`). That license applies to **this repository's own source**. The
components below are not relicensed by it, and each keeps its own terms.

## The crates the build resolves

`rust/Cargo.lock` pins **307** third-party crates (plus the workspace's own
crates). Of those, **234** are present in this machine's Cargo cache and were
read from the `license` field of the manifest of the exact pinned version:

| crates | license |
| ---: | --- |
| 108 | MIT OR Apache-2.0 |
| 39 | MIT |
| 20 | Apache-2.0 OR MIT |
| 18 | Unicode-3.0 (unicode data tables) |
| 9 | MIT/Apache-2.0 |
| 5 | Zlib OR Apache-2.0 OR MIT |
| 4 | Apache-2.0 |
| 3 | MIT OR Apache-2.0 OR Zlib |
| 3 | ISC |
| 2 | Unlicense OR MIT |
| 2 | Zlib |
| 2 | MIT OR Zlib OR Apache-2.0 |
| 2 | BSD-3-Clause OR Apache-2.0 |
| 2 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| 2 | BSD-2-Clause OR Apache-2.0 OR MIT |
| 1 each | 0BSD OR MIT OR Apache-2.0 · MIT / Apache-2.0 · Apache-2.0/MIT · Apache-2.0 AND MIT · (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 · MIT AND BSD-3-Clause · MIT OR Apache-2.0 OR LGPL-2.1-or-later · Apache-2.0 AND ISC · Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT · Apache-2.0 OR ISC OR MIT · Apache-2.0 OR GPL-2.0-only · BSD-3-Clause · CDLA-Permissive-2.0 |

That is 234 of 307. The other **73** are crates this machine has never needed
because they belong to other platforms — Android and Wayland backends
(`android-activity`, `calloop`, `smithay-client-toolkit`), Windows GL loaders
(`gl_generator`, `glutin_wgl_sys`), and their kin. Their licenses are not
counted above because their manifests are not in the local cache; every one of
them is a platform backend of `winit`/`glow`, and none of them is linked into a
binary this repository builds here. The rule for the whole tree is the one
`atlas-guard` enforces on `Cargo.lock`: permissive only, pinned exactly, no
`git` source, no second registry.

Most of the entries offer a choice (`OR`): ATLAS takes the Apache-2.0 side where
one is offered, which is why one license text covers this repository. Every
license text ships inside the crate that declares it, in the local Cargo
registry.

## The model server

`tools/llama.cpp` is vendored and keeps its own **MIT** license, its own
`LICENSE` file and its own copyright notices. The launcher builds it and starts
it as a child process; ATLAS does not relicense it.

## The model

The GGUF weights are **not** part of this repository and are not licensed by it.
They are fetched by `atlas setup` from the address the reader chooses and remain
under the terms of the model's author.

## News content

Nothing in this repository licenses any article, image or page text. The app
stores a summary the local model wrote, the article's own URL and the metadata
around it; the publisher's terms still apply to the article itself.

## Re-deriving this file

```sh
cd rust
cargo metadata --format-version 1 > /tmp/meta.json   # needs the crates cache
python3 - <<'PY'
import json, collections
m = json.load(open('/tmp/meta.json'))
third = [p for p in m['packages'] if p.get('source')]
counts = collections.Counter(p.get('license') or 'unknown' for p in third)
print(f'{len(third)} third-party crates')
for license, n in counts.most_common():
    print(f'{n:4d}  {license}')
PY
```
