# Response contract

Wayfinder serves JSON only. Normal search-style replies have `responseHeader`
with numeric `status`, `QTime`, and raw-string echoed `params`, plus `response`
with `numFound`, `start`, `numFoundExact`, and `docs`. `numFoundExact` is always
true. Requested components add their own blocks (`facet_counts`, `highlighting`,
`stats`, groups, and bounded helper payloads). Object key order is not semantic;
unknown `fl` fields are omitted.

Error families use the matching HTTP status, `responseHeader.status`, and
`error.code`, with a nonempty `error.msg`. The update/extract family does not
echo parameters on its error form; other routes can use a with-params family;
unsupported methods can have the bare family. Typical boundaries are 400 syntax,
validation, or strict parameter failure; 401 Basic failure; 404 unknown core;
413 body budget; 415 format; 500 parser; and 503 extraction saturation/deadline.

`omitHeader=true` removes a normal header only on handlers that support it;
invalid values are JSON 400 rather than HTML. `strict_params=true` rejects
unlisted names but not a supported parameter merely because its behavior is
constrained. Accepted does not imply effect: `query.time_allowed` and
`resources.searcher_pool_size` are inert. `rows` and facet limits clamp rather
than reject; use the returned pagination/count and request a supported next
page. A successful response can carry `responseHeader.warnings` when Wayfinder
constrains a request—for example, raising a Points-based facet's minimum count.
Unknown parameters are silently ignored when strict mode is off.

## Safe interpretation

Prerequisites: choose JSON and a known core, then parse status before consuming
payloads. Visibility and durability describe the underlying committed index,
not the envelope; a successful read never changes either. On failure, retain
the request/correlation evidence, correct credentials/core/grammar/budget and
retry only when no mutation was involved. Validate status, family, key presence,
and business result—not QTime or object order. Rollback for a read is discard;
for an update error, reconcile the document because a partial valid prefix may
exist before retrying.

Exact envelope shapes are covered by `tests/error_shapes.rs`, `tests/omit_header.rs`,
`tests/select_warnings.rs`, `tests/body_limit.rs`, and `tests/update_pipeline.rs`.
