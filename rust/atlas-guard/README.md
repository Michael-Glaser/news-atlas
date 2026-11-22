# atlas-guard

Small, dependency-free Rust CLI that holds every security decision ATLAS makes about
untrusted input: one JSON object in on stdin, one JSON object out on stdout.
Bad input prints `{"error":"..."}` and exits 1; success exits 0. Build with
`cargo build --release` (Rust 2021, std only, `unsafe` forbidden, no network, no
filesystem access). See `docs/SECURITY-RUST.md` for the policy behind it.

A *verdict* is not an error: `url`, `token` and `address` answer with `ok:false` and a
`reason` and still exit 0, so the caller can act on the reason. Only a malformed
request (bad JSON, missing field, oversized field) exits 1.

| subcommand | input | output |
| --- | --- | --- |
| `url` | `{url ≤2048, allow_private?:false}` | `{ok, scheme, host, port, reason, is_private_ip}` |
| `clean_text` | `{text ≤1 MiB, max_chars?}` | `{text, removed, truncated}` |
| `sanitize_html` | `{html ≤1 MiB}` | `{text}` |
| `token` | `{value, kind:"tag"\|"place"\|"topic"\|"search"}` | `{ok, value, reason}` |
| `redact` | `{text ≤1 MiB}` | `{text, hits}` |
| `address` | `{value}` | `{ok, loopback, kind, reason}` |

## url — the SSRF gate
in : `{"url":"https://example.com/a","allow_private":false}`
out: `{"ok":true,"scheme":"https","host":"example.com","port":443,"reason":"","is_private_ip":false}`
in : `{"url":"http://169.254.169.254/latest/meta-data/","allow_private":false}`
out: `{"ok":false,"scheme":"http","host":"169.254.169.254","port":80,"reason":"address is not publicly routable (private, loopback, link-local, documentation or reserved)","is_private_ip":true}`

Only `http`/`https`, no userinfo, host required, ≤2048 characters, no whitespace or
control characters, and no invisible character (a zero-width, a bidi override or a tag
character — an instruction a person cannot see; `cleantext::is_invisible` is the rule).
Refused hosts:

* **IPv4 literals in these ranges** — 0/8, 10/8, 127/8, 169.254/16, 172.16/12, 192.168/16,
  100.64/10 (CGNAT), 192.0.0/24, 192.0.2/24 (documentation), 198.18/15 (benchmarking),
  198.51.100/24, 192.88.99/24 (6to4 relay anycast, RFC 7526), 203.0.113/24 (documentation), and
  anything ≥224 (multicast, reserved, broadcast).
* **IPv6 literals in these ranges** — `::`, `::1`, fc00::/7, fe80::/10, fec0::/10, ff00::/8,
  100::/64 (discard-only), 2001::/32 (Teredo), 2001:db8::/32 and 3fff::/20 (documentation),
  2001:2::/48 (benchmarking, RFC 5180), 2001:10::/28 (ORCHID, RFC 4843) and 2001:20::/28
  (ORCHIDv2, RFC 7343). None of them can be a publisher, and each prefix's neighbours stay
  public — `2001:30::1`, `2001:2:1::1` and `3fff:1000::1` are accepted.
* **Any literal that *carries* an IPv4 address a translator would deliver to** — `::a.b.c.d`,
  `::ffff:a.b.c.d`, `::ffff:0:a.b.c.d`, `64:ff9b::a.b.c.d`, `64:ff9b:1::a.b.c.d` and
  `2002:aabb:ccdd::` (6to4) — judged by the IPv4 rules, so `[64:ff9b::7f00:1]` is refused like
  `127.0.0.1`. The embedded address counts **only in the final 32 bits**, which is the only place
  RFC 4291 puts one: `1.2.3.4::`, `127.0.0.1::1` and `127.0.0.1::ffff:127.0.0.1` are not literals
  any resolver accepts, so they are refused as malformed rather than read as a public page
  (`tests/fuzz_url.rs` pins this parser against the platform's, and this was the one disagreement
  that measurement found).
* **Names that resolve inside a machine or a LAN** — `localhost`, `local`, `localdomain`,
  `internal`, `intranet`, `lan`, `corp`, `private`, `home.arpa`, `home`, `mail`, `ip6-localhost`,
  `ip6-loopback`, `broadcasthost`, and the RFC 2606/6761 names `test`, `example`, `invalid`. A
  subdomain of any of them is refused too (`printer.home`, `smtp.mail`, `localhost.evil`), with or
  without the trailing root dot a resolver would add. `home` and `mail` were the two the list was
  missing: no registry delegates them, and every router that hands out `host.home` or `mail.corp`
  expects its own resolver to answer. Vendor-only names (`fritz.box`, `workgroup`) are deliberately
  *not* listed — that list would never be finished, and a name that is not in a registry is judged
  by its shape here, and by the DNS answer `atlas-net`'s resolver checks after it.
* **Any host every label of which reads as a C number**, because `inet_aton` expands those
  (`127.1`, `2130706433`, `0177.0.0.1`, `0x7f.0.0.1`, `0x7f000001`, `127.0x1`); a host whose last
  label is all digits; and any host with no dot at all, because a bare name resolves through the
  machine's search list — inside the LAN.
* **A bracketed address followed by anything but `:port`** — `http://[::1]junk/` is "unexpected
  characters after the ipv6 literal", and `http://example.com:80:90` is "the authority has more
  than one colon" rather than being read as an IPv6 literal.

`port` is the effective port (the default 80/443 when the URL omits one). Hosts must be
ASCII: punycode is decoded for the blocklist check, so an encoded name cannot hide behind
it, and the returned `host` is the ASCII form actually dialled. `allow_private:true` is for
the app's own loopback calls and never for a fetched or feed-supplied URL.

## clean_text — what may reach the DB, a log line or a prompt
in : `{"text":"line\u0000 one\n\n\n\nsecond  ","max_chars":0}`
out: `{"text":"line one\n\nsecond","removed":1,"truncated":false}`
Drops control characters (C0/C1) except `\n` and `\t` and counts them in `removed`;
drops zero-width and bidi-override characters (U+200B–200F, U+202A–202E, U+2060–2064,
U+2066–2069, U+FE00–FE0F, U+FEFF, U+FFF9–FFFB, U+E0000–E007F, U+E0100–E01EF, U+180E,
U+00AD) — the last two ranges are the tag characters and variation selectors that spell
invisible ASCII, i.e. instructions a model reads and a person cannot; maps NBSP and friends
to a space; turns CRLF into `\n`; collapses three or more newlines to `\n\n` and keeps
single line breaks; trims line ends and the edges.
`max_chars` (0 or absent = no limit) truncates at a paragraph boundary, else at a word
boundary, and sets `truncated`. Text over 1 MiB is cut at 1 MiB and reported as truncated.

## sanitize_html — a page, before any model reads it
in : `{"html":"<p>Hi <b>there</b></p><script>alert(1)</script><p>Second</p>"}`
out: `{"text":"Hi there\n\nSecond"}`
Drops tags, comments, declarations and the whole content of script/style/template/svg/head/
iframe/object/embed/noscript/noframes/canvas/audio/video/form/select/textarea; block
elements start a paragraph (`\n\n`); entities are decoded; every paragraph is cleaned with
`clean_text`'s rules; output is capped at 200 000 characters; a `<` that is not a tag stays
as text. HTML over 1 MiB is cut at 1 MiB before scanning.

## token — a tag, a place, a topic, a search term
in : `{"value":"  NVIDIA   Corp ","kind":"tag"}`
out: `{"ok":true,"value":"nvidia corp","reason":""}`
The value may also arrive as `"text"` (the field name the request adapter uses for every
operation), so a field-name mismatch cannot silently leave that path switched off.
Lowercases, trims, collapses spaces; max 40 (`tag`), 80 (`place`, `topic`), 200 (`search`)
characters; letters, digits, space, `-`, `'`, `&`, `.` only; no `%`/`_` (SQL wildcards), no
control or invisible characters, no leading or trailing punctuation. The normalised value is
returned even when rejected. Unknown `kind` is bad input (exit 1), not a verdict.

## redact — before anything is logged or prompted
in : `{"text":"Authorization: Bearer abcdef123456 mail analyst@example.com"}`
out: `{"text":"Authorization: Bearer [redacted] mail [email]","hits":2}`
Removes PEM private-key blocks (an unterminated block to end of text), prefixed API keys
(`sk-`, `sk_live_`, `ghp_`, `github_pat_`, `AKIA`, `AIza_`, `xoxb-`, `glpat-`, `hf_`, `SG.`,
JWTs starting `eyJ`, …), `authorization`/`bearer` values including the credential after a
scheme word (`Authorization: Basic …` keeps the word and loses the value),
`password`/`secret`/`token`/`api_key` assignments — including a name that carries the word
after an underscore (`db_password=`, `AWS_SECRET_ACCESS_KEY=`, `token_id=`) — with quoted
values always, `name=value` values whatever they look like, and `name: value` values only
when they look like secrets so prose survives, and e-mail addresses. Overlapping spans
merge, so one secret is one hit and no fragment of it survives. Text over 1 MiB is refused
instead of truncated.

## address — is this listener loopback?
in : `{"value":"127.0.0.1"}` → `{"ok":true,"loopback":true,"kind":"ipv4","reason":""}`
in : `{"value":"0.0.0.0:9222"}` → `{"ok":true,"loopback":false,"kind":"wildcard","reason":"wildcard address listens on every interface"}`
`loopback:true` only for 127/8, `::1`, `::ffff:127.x`, `localhost`. `0.0.0.0`, `::`, `*`, a
LAN address, a link-local address or a host name answer `loopback:false` — an address that
cannot be proven loopback never passes as safe. The `addr:port`, `[addr]:port` and
`addr:*` forms that lsof and netstat print are accepted (`*:*` is the wildcard, not an
error), and a `%zone` suffix is ignored. This is the decision the listener audit and the
browser's debug-port check ask for; neither of them classifies addresses itself.

## Limits worth knowing
The gate is a pre-flight check on the URL text: it does not resolve DNS, follow redirects or
inspect TLS. Both of those follow-ups are someone else's, on purpose: `atlas-net` re-gates every
redirect hop, and its resolver filters the addresses the connector is given
(`atlas-net/src/resolver.rs`), so a name whose answer changes between the verdict and the connect
does not connect on the HTTP path. The reader browser is the exception — Chrome resolves for
itself — and `docs/SECURITY-RUST.md` §9 states that as an open residual rather than implying the
guard covers it. IDN hosts must arrive as punycode; non-ASCII is refused rather than rewritten.
No NFKC folding (it would change the text a model reads).
