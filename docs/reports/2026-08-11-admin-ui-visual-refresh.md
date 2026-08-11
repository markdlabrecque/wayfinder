# Admin UI visual refresh (2026-08-11)

No issue behind this one: a direct request to make the admin UI look better while
keeping it accessible and easy to read. Presentation only — no handler, route, or
data change.

## What changed

Before, each of the six `/ui*` pages was a standalone HTML document with its own
~6-line `<style>` block and no way to get from one page to another. Now they all
extend a shared layout.

- `templates/_layout.html` (new) — the document shell: design tokens as CSS
  custom properties, a light and a dark theme via `prefers-color-scheme`, the
  masthead (compass mark, `Wayfinder`, core chip), an inline SVG favicon, the
  skip link, and the whole stylesheet. Card panels, key/value fact tables,
  record listings with sticky headers, callouts, badges, form controls.
- `templates/_nav.html` (new) — the section nav as an Askama macro taking the
  active page key, so each page marks its own tab with `aria-current="page"`.
  This is the one behavioural addition: the six pages are now reachable from
  each other.
- `templates/{core,ping,query,schema,stats,synonyms}.html` — rewritten as
  `{% extends %}` children. Same figures, same prose, same form fields and
  synonym-editor script.

The stylesheet is inlined in the layout rather than served as a static asset:
this is a handful of server-rendered pages with no asset pipeline, and a
`/ui/style.css` route would be new request surface for no gain at this size.

## Markup the tests pin

Two places where the tests constrain what the markup may look like, both now
commented where they apply:

- `templates/schema.html`: the schema-view tests parse rows by splitting on the
  literal `<td>` and compare cell text exactly (`yes`/`no`), and read
  `<th scope="col">` labels verbatim. Cells therefore carry no attributes;
  styling hangs off the table's class and column position.
- `templates/ping.html`: `word_after_label` takes the first word after `status`,
  so the label and value share one element with no tag between them
  (`<p class="badge ...">Status: OK</p>`). A `<span>` around the value would be
  read as the value. For the same reason the layout's stylesheet comment says
  "Notes and badges", not "Notes and status" — the CSS text is part of the body
  the tests scan.

## Accessibility

Heading order unchanged (`h1` brand, `h2` page, `h3` sections); `scope` on every
header cell; visible `:focus-visible` outlines; skip link to `#main`;
`aria-label` on the nav and `aria-current` on the active tab; decorative SVG and
status dot marked `aria-hidden`. Palette checked for AA contrast in both themes.
Responsive to 380px: wide tables scroll inside their own container and the
document itself does not scroll horizontally (verified,
`documentElement.scrollWidth == clientWidth` at 380px).

## Evidence

- `cargo test`: 1483 passed, 1 ignored, 0 failed (75 suites).
- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.
- Rendered in Chromium at 1280px and 420px, light and dark, on all six pages,
  against a core built from `presets/search-api.toml` with three indexed
  documents and two synonym groups.

## New tests, and their ordering

`tests/admin_ui_layout.rs` covers the shared chrome the per-page suites never
saw: every page links to all six sections, each page marks exactly its own tab
current, and every page has a skip link with a target. These were written after
the layout, not red-first — the refresh was a visual refactor of pages the
existing suites already pinned, and the nav was the one new behaviour. Each
assertion was mutation-checked instead: pointing `/ui/stats` at a nonexistent
route, marking every tab current, and deleting the skip link each turn this file
red, and reverting each turns it green.

## Follow-ups

- Boolean columns in the schema tables read as plain `yes`/`no` text. A
  checkmark/dash treatment would scan faster but needs a per-cell class or
  element, which the schema tests' `<td>`-splitting parser forbids; changing that
  means changing those tests too, deliberately, not as a drive-by.
- The query tester's response payload is unhighlighted `<pre>`. Syntax colouring
  would need either client-side JS or server-side tokenising of the JSON.
