# Wire and UI route inventory

Source provenance: `search_api_routes!` and the UI `.route(...)` declarations in
[`src/lib.rs`](../../src/lib.rs). The wire routes retain bounded Solr-shaped JSON;
`Any` records the router declaration, not an assertion that every method has the
same handler semantics. See [Compatibility](../../docs/COMPATIBILITY.md).

## Wire routes

| Route | Router method | Purpose |
|---|---|---|
| `/wayfinder/{core}/update` | Any | Add, delete, and commit whole documents; the handler validates update methods. |
| `/wayfinder/{core}/update/extract` | Any | Multipart extraction or extraction-backed indexing. |
| `/wayfinder/{core}/select` | Any | Search and supported components. |
| `/wayfinder/{core}/mlt` | Any | MoreLikeThis. |
| `/wayfinder/{core}/terms` | Any | Terms lookup and prefix completion. |
| `/wayfinder/{core}/suggest` | Any | Dictionary suggestion operations. |
| `/wayfinder/{core}/admin/ping` | Any | Core health. |
| `/wayfinder/admin/info/system` | Any | Server information. |
| `/wayfinder/{core}/admin/system` | Any | Core-scoped server information. |
| `/wayfinder/{core}/schema/fieldtypes` | Any | Field-type metadata. |
| `/wayfinder/{core}/admin/luke` | Any | Schema and index metadata. |
| `/wayfinder/{core}/admin/mbeans` | Any | Selected runtime metrics. |

## Wayfinder UI routes

The UI is outside the retained wire. `GET /ui`, `GET /ui/query`, `GET /ui/schema`,
`GET /ui/stats`, and `GET /ui/ping` are read-only. `GET /ui/synonyms` reads the
current query-synonym groups. `POST /ui/synonyms` validates the complete form and
atomically replaces durable `<data-dir>/synonyms.txt`; it hot-swaps query analysis
only and does not reindex documents.

| Route | Method | Classification |
|---|---|---|
| `/ui` | GET | Read-only core overview. |
| `/ui/synonyms` | GET; POST | GET is read-only synonym-group view; POST atomically replaces durable query synonyms. |
| `/ui/query` | GET | Read-only query tester. |
| `/ui/schema` | GET | Read-only schema view. |
| `/ui/stats` | GET | Read-only live index statistics. |
| `/ui/ping` | GET | Read-only health check. |
