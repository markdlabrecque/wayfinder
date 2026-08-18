# Search queries and result windows

Search uses `GET` or supported request forms on `/select` with a bounded parser,
not every Solr parser. See [Compatibility](../../docs/COMPATIBILITY.md), the
[parameter inventory](../reference/parameters.md), and hermetic advanced shapes
in [`tests/query_types.rs`](../../tests/query_types.rs).

## Query syntax, filters, paging, sorting, and fields

`q` supports Boolean, term, phrase, prefix, range, fuzzy, and regex queries.
Repeatable `fq` clauses filter results. `start` and `rows` select a window;
`rows` is clamped by the configured limit. `sort` supports compatible fast
fields and multi-clause ordering. `fl` returns stored fields; unknown `fl`
fields are omitted. Exact counts remain `numFoundExact=true`. Make a field
`fast` before designing a sort or facet around it.

A normal JSON envelope has `responseHeader` and `response`; errors have matching
HTTP/header/error codes. `wt=json` is the only writer. Unknown parameters are
ignored unless `strict_params=true`, when names outside the per-route allowlist
are 400. This is validation, not implied parity; inspect a parameter's
implemented, **constrained**, **inert**, or **warning-only** status in the
inventory. Response details are in [response errors](../reference/response-errors.md).

## Edismax, local parameters, functions, and payload scores

The constrained edismax subset includes `defType`, `qf`, `pf`, `mm`, `tie`,
`bq`, `bf`, quoted phrases, signs, and `boost`. `pf2`, `pf3`, `ps`, `stopwords`,
and `lowercaseOperators` are unsupported edismax parameters. Function support
is constants, numeric fields, `sum`, `product`, `max`, `min`, and `recip`.
Only `{!edismax}`, `{!func}`, and `{!boost ...}` are local-param parser names;
`{!lucene}` is a `SyntaxError` 400.

`{!payload_score}` is constrained to one payload-bearing term with
`includeSpanScore=false`; querying a non-payload field is 400. It requires the
specialized schema type, not a regular text field. Treat low-recall historical
local-parameter shapes as preserved client behavior, not a cue to broaden the
parser. Advanced parser/function/payload requests are exercised in
[`tests/edismax.rs`](../../tests/edismax.rs), [`tests/local_params.rs`](../../tests/local_params.rs),
and [`tests/payload_score.rs`](../../tests/payload_score.rs).

## Strict limits and failure handling

Before a high-cost query, establish compatible fields and configured row/facet
limits. A malformed request is safely retryable only after correction; a timeout
setting is currently **inert**, so it is not a cancellation guarantee. Validate
with `responseHeader.status`, `numFound`, selected IDs, and component blocks.
Search is read-only: rollback means discard the request or issue a corrected
one, not an index repair.
