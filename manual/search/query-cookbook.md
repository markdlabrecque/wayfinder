# Query cookbook

Use `q` for the bounded Lucene-shaped grammar: `*:*`, terms, fielded terms,
quoted phrases, boolean `+`/`-`/`OR`, prefix/wildcard (`*`, `?`), fuzzy
`term~N`, regex `/pattern/`, ranges (`[a TO b]`, `{a TO b}`, half-open), and
numeric/date values typed by their field. Regex is whole-term and case-sensitive;
wildcard/fuzzy lower-case but do not stem. A field-exists query is `field:*` for
supported static fields. Put repeatable restrictions in `fq`. `start`/`rows`
page exact results; use an explicit multi-clause `sort` (with a unique-key
clause when repeatable page boundaries matter). `fl` supports named stored
fields, `*`, dynamic-name wildcards, and `score`; unknown fields are omitted.
Sorting and faceting require compatible fast fields. `q.op` and `qt` are
unsupported, and `wt=json` is the only writer.

## Relevance and local parameters

Default ranking is BM25; score keys/order are meaningful but raw Solr float
parity is not. `defType=edismax` supports `q`, `qf` with boosts, `pf`, `mm`,
`tie`, `bq`, `bf`, `boost`, quoted phrases, and `+`/`-`. `pf2`, `pf3`, `ps`,
`stopwords`, and `lowercaseOperators` are unsupported. An empty multi-clause
`mm` is invalid; absence uses the normal default.

Function expressions are constants, numeric fields, `sum`, `product`, `max`,
`min`, and `recip`; spatial result fields/sorts also support `geodist()` with a
static `location`/`location_rpt` field plus `sfield` and `pt`. Coordinates are
`latitude,longitude`, not GeoJSON longitude-first order. `{!func}` and
`{!boost b=...}` use the bounded numeric evaluator. Local parser names are only
`edismax`, `func`, and `boost`; `{!lucene}` and other names return SyntaxError
400. Inline local parameters bind only their next token in the captured Shape-B
behavior—parser placement and parentheses can therefore change scope; do not
rewrite them as a higher-recall whole query.

`{!payload_score f=... v=... func=max|min|average|sum includeSpanScore=false}`
requires a `boost_term_payload` field and one payload-bearing term. `v` is
analyzed; a missing/empty term, unsupported function, or non-payload field is
400. `includeSpanScore=false` is the supported mode. Position-zero blocks have
the documented bounded parsing behavior; parenthesize an inline block before
boosting it. This is not general span/payload syntax.

## Request discipline

Queries are read-only: prerequisites are committed documents and a schema whose
field/type can satisfy the syntax. Visibility and durability are the committed
searcher view; query success changes neither. On failure, correct the grammar,
field, or bounds and retry; validate IDs/count/order against known data.
Rollback means discarding the result, not mutating the index.

Advanced request shapes are hermetically specified by `tests/query_types.rs`,
`tests/edismax.rs`, `tests/local_params.rs`, `tests/payload_score.rs`, and
`tests/mm.rs`.
