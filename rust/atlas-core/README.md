# atlas-core
Small, dependency-free Rust CLI for the ATLAS pipeline: one JSON object in on stdin, one JSON object out on stdout.
Bad input prints {"error":"..."} and exits 1; success exits 0. Build with cargo build --release (Rust 2021, std only).

## extract — readable article text out of a full HTML page (JSON-LD articleBody, else largest <article>, else longest <p> run, else the page's visible text).
in : {"html":"<article><p>Chip demand keeps rising across the whole supply chain.</p></article>","url":"https://e.com/a","title":"Chips"}
out: {"title":"Chips","text":"Chip demand keeps rising across the whole supply chain.","chars":55}
script/style/nav/header/footer/aside/form/iframe are dropped first; entities are decoded; a page whose element is never
closed does not lose the rest of the document.

## entities — count proper-noun phrases plus the caller's known names.
in : {"text":"Nvidia and TSMC rose.","known":["Nvidia"],"min_count":1,"max":40}
out: {"entities":[{"name":"Nvidia","count":1,"known":true},{"name":"TSMC","count":1,"known":false}]}
A fragment that only ever occurs inside a longer phrase is reported once, as part of that phrase
("Nvidia Corp", not also "Corp"). Known names that never occur are not reported.

## similar — cosine similarity over sparse token vectors, top pairs per document.
in : {"docs":[{"id":1,"tokens":{"nvidia":0.8,"chip":0.6}},{"id":2,"tokens":{"nvidia":0.6,"chip":0.8}}],"top_per_doc":8,"min_score":0.25}
out: {"pairs":[{"a":1,"b":2,"score":0.96}]}
