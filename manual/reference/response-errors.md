# Response and error reference

The retained wire is JSON-only. A normal search response has `responseHeader`
with `status`, `QTime`, and raw echoed `params`, then a `response` object with
`numFound`, `start`, `numFoundExact`, and `docs`. Requested components add their
own blocks such as `facet_counts`, `highlighting`, and `stats`. `numFoundExact`
is always `true`.

An error is JSON with matching HTTP status, `responseHeader.status`, and
`error.code`:

```json
{"responseHeader":{"status":400},"error":{"msg":"...","code":400}}
```

`omitHeader=true` removes the normal response header where that route supports
it. Parameter echoes are raw strings and object key order is not semantic.
Common request failures are 400 for invalid syntax, validation, or strict
parameters; 401 for failed Basic authentication; 404 for an unknown core; 413
for a body or extraction-content limit; 415 for an unsupported extraction
format; 500 for an extraction/parser failure; and 503 for exhausted extraction
budgets or deadlines. See the normative [Compatibility](../../docs/COMPATIBILITY.md)
contract and [extraction boundary](extraction.md).
