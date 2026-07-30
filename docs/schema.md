# Wayfinder schema file

One TOML file per core, passed to `wayfinder::app(schema_path, data_dir)`. PRD §3 is the
design rationale; this is the reference.

```toml
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
fast = true              # required for facet / sort
multi_valued = true

[[fields]]
name = "created"
type = "date"
fast = true

[[dynamic_fields]]
pattern = "*_i"
type = "int"
stored = true
fast = true

[[copy_fields]]
source = "title"
dest = "body"
```

## Field options

| Option | Meaning |
|---|---|
| `stored` | Value is retrievable in `fl` / returned in `response.docs`. |
| `required` | A document missing this field is rejected. |
| `fast` | Tantivy fast field — **required** to facet or sort on the field. |
| `multi_valued` | Rendered as a JSON array in responses, even with a single value. |

## Field types

| Type | Notes |
|---|---|
| `string`, `keyword` | Not analyzed — one exact term. |
| `text_general` | Tokenized, lowercased, not stemmed. |
| `text_en` | `text_general` plus an English stemmer. |
| `text_<code>` | Same shape, stemmed for that language. Codes: `ar da nl en fi fr de el hu it no pt ro ru es sv ta tr`. |
| `int`, `long` | 64-bit signed integer. |
| `float`, `double` | 64-bit float. |
| `date` | RFC3339 in UTC, e.g. `2026-07-28T12:00:00Z`. |

Language presets stem but do **not** remove stopwords, matching Tantivy's own `en_stem`. For
stopword removal, declare a custom chain.

## Custom analyzer chains

The escape hatch, when a preset will not do:

```toml
[[field_types]]
name = "text_en_custom"
tokenizer = "simple"          # the only tokenizer for now
[[field_types.filters]]
kind = "lowercase"
[[field_types.filters]]
kind = "stopwords"
language = "english"          # name or ISO-639-1 code
[[field_types.filters]]
kind = "stemmer"
language = "english"
```

Filters apply in declared order. Kinds: `lowercase`, `stopwords`, `stemmer`. Tantivy ships no
stopword list for Arabic, Greek, Romanian, Tamil or Turkish — a `stopwords` filter in those
languages is a load-time error rather than a silent no-op.

Use the type by name: `type = "text_en_custom"`.

## Dynamic fields

`pattern` is a Solr-style glob with a `*` at exactly one end, or a bare `*` (`*_i`, `title_*`,
`*`). Anything else — `*_i*`, `a*b` — is rejected at load time rather than given semantics Solr
does not have.
The **longest matching pattern wins**, and a field declared in `[[fields]]` always beats a
pattern that would also match it.

A document field matching no static field and no pattern is rejected the way a non-schemaless
Solr rejects it (HTTP 400, `unknown field '<name>'`). Wayfinder never adds fields to a schema
at runtime; the `_default` Solr configset's schemaless auto-add is deliberately not copied
(PRD §3). Both behaviours are captured in
`solr-ref/responses/update_unknown_field_{strict,schemaless}.json`.

Implementation note: Tantivy schemas are fixed at index creation, so dynamic fields cannot each
become a Tantivy field. Their values live in two catch-all JSON fields, `_dynamic` (unanalyzed
types) and `_dynamic_text` (analyzed types), and queries on a dynamic field name are rewritten
to the matching JSON path. The containers never appear in a response — stored dynamic fields
come back as ordinary top-level keys.

## Copy fields

```toml
[[copy_fields]]
source = "title"
dest = "body"
```

Applied at index time: `dest` receives `source`'s raw value and analyzes it with its own field
type. Many sources may target one destination, and `dest` need not be `stored`. Both endpoints
must be declared fields. Changing `[[copy_fields]]` only affects documents indexed afterwards.

## Schema changes and the startup check

The schema an index was built with is stored next to it as `<data_dir>/wayfinder-schema.toml`.
On startup Wayfinder compares the configured schema against it and **refuses to open** on any
change to `[[fields]]`, naming the field and saying a reindex into a fresh data directory is
needed (PRD open question 4). This includes *adding* a field: the PRD calls that compatible, but
Tantivy cannot extend an existing index's schema in place, so v1 still requires a reindex.

`[[copy_fields]]` and `[[field_types]]` never change the Tantivy schema — they govern index-time
content and analysis — so they may be edited freely, and the change applies to documents indexed
from then on.

`[[dynamic_fields]]` is *almost* in that category. Editing, adding or removing rules while at
least one rule remains changes nothing structural. But the catch-all JSON fields exist only when
there is at least one rule, so **adding the first rule or removing the last one changes the
Tantivy schema** and is refused like a field change:

```
[[dynamic_fields]] went from 0 rule(s) to 1; the existing index has no catch-all field to hold
their values — reindex into a fresh data directory
```

## Presets

`presets/search-api.toml` ships the Drupal `search_api_solr` module's field-naming convention
(prefixes like `ss_`, `sm_`, `tm_X3b_en_`, `its_`, `ds_`, `bs_`) as a ready-made schema, so a
Drupal site can point at Wayfinder with `wayfinder presets/search-api.toml <data-dir>` and no
per-site schema authoring. See the file's own header comment for the couple of documented
divergences (no native boolean type; `sort_*` stands in for Solr's collation type).
