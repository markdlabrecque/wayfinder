# Discovery helpers: MLT, terms, spellcheck, and suggest

Choose the helper that matches the user outcome rather than assuming interchangeability.
Their routes and bounded parameters are listed in [wire routes](../reference/wire-routes.md)
and [parameters](../reference/parameters.md); advanced request behavior is
covered by [`tests/mlt.rs`](../../tests/mlt.rs), [`tests/terms.rs`](../../tests/terms.rs),
[`tests/spellcheck.rs`](../../tests/spellcheck.rs), and
[`tests/suggest.rs`](../../tests/suggest.rs).

## Choose the right helper

Use **MoreLikeThis** (`/mlt`) to find documents similar to supplied/identified
content under its configured fields. Use **terms** (`/terms`) for indexed term
inspection or prefix completion. Use **spellcheck** when a search spelling
alternative is useful. Use **suggest** (`/suggest`) for configured dictionary
suggestions; repeated `suggest.dictionary` values receive separate response
keys and `suggest.highlight=false` disables server-side highlighting.

None is a shipped autocomplete endpoint. Drupal's stock `/autocomplete`
expectation is unsupported; see [Drupal integration](../integrations/drupal.md).
They also do not create an external dictionary, OCR source, or unbounded Solr
component.

## Safe read-only lifecycle

Prerequisites are committed source documents and fields appropriate to the
helper; terms/suggestions are only as current as the committed index and
configuration. These requests do not mutate state or make writes durable.
Validate returned IDs/terms and the JSON envelope against known inputs. On 400,
correct the bounded grammar; on 401/404, correct credentials/core; then retry.
There is no rollback because helpers are read-only. Status/error meanings are in
[response errors](../reference/response-errors.md).
