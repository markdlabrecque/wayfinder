> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #362 — N+1 identical sort copies written per text field

Branch: `362-sort-copy-fanout`. Group A.

**This is a measurement task first. It may correctly end with no code change.**
Do not start by writing an optimisation.

## The situation

`DocumentBuilder` writes a text field's `sort_*` copy once per enabled site
language plus `und` — see `DocumentBuilder.php:149` and the language list at
`:170`. This mirrors `SearchApiSolrBackend.php:1469-1481`, whose comment is "To
allow sorted multilingual searches we need to fill *all* language-specific sort
fields!".

The values written are **identical**; only the field name differs. On a site
with many languages this multiplies stored sort fields per text field.

The behaviour is *correct as compatibility behaviour*. It is what Solr does, it
matches the captured trace, and #342 built it deliberately. The question is
purely whether the storage cost justifies a Wayfinder-side divergence to a
single sort field with language-agnostic naming.

## What to do

**Step 1 — measure.** This is the deliverable, and it is most of the work.

Build a representative index and quantify the cost. Vary the language count (1,
2, 5, 10) against a realistic text-field count and document volume. Report:

- stored index size, total and attributable to `sort_*` fields
- how the cost scales — linear in languages as expected, or worse
- query-time effect, if any (these are fast fields; establish whether the fan-out
  costs anything at query time or only on disk)
- the same numbers for the single-field alternative, so the comparison is real
  rather than assumed

Use whatever the repo already has for index-size measurement before building
something new — check `src/core_index.rs` and the admin stats path
(`src/admin_ui.rs`, `tests/admin_ui_index_stats.rs`) first.

**Step 2 — decide, and write the decision down.** If the cost is negligible at
realistic language counts, the correct outcome is: **keep the current behaviour,
record the measurement in `docs/reports/`, and close the issue.** That is a
successful result, not a failure to deliver. Compatibility is the default and
the burden of proof is on diverging from it.

If the cost is material, propose the divergence — but do not implement it in
this branch. Write up what would change, what breaks, and how sorting would
resolve a language-agnostic field, and open a follow-up issue. A divergence from
captured Solr behaviour needs PRD sanction before it is built; that is the
compatibility contract, and this issue does not carry it.

## The trap

A single language-agnostic sort field is not obviously safe. `search_api_solr`'s
comment says the fan-out exists specifically so *sorted multilingual searches*
work. Before concluding the copies are redundant, establish what breaks when a
query sorts on `sort_X3b_de_field_x` and only a language-agnostic field exists —
including whether the client ever sends a language-specific sort field name that
Wayfinder would then have to rewrite. **If the client sends the language-specific
name, a single field is not a storage optimisation, it is a wire-level
divergence with a translation layer attached.** Say so if you find it.

## Verify before measuring

1. Confirm the values really are always identical across languages. Read
   `DocumentBuilder.php:140-155` and the `sortLanguages` method at `:170`. If any
   path writes *different* values per language, the whole premise is wrong and
   the issue stops here.
2. Confirm what the client sends as a `sort` param — language-specific field
   names or not. Grep `QueryBuilder.php` and the captured traces.

## Files

**You own:** `docs/reports/YYYY-MM-DD-362-sort-copy-fanout.md` (the measurement),
and `DocumentBuilder.php` only if step 2 concludes a change is warranted **and**
the divergence is sanctioned first.

**Siblings own:** #358 also touches `DocumentBuilder.php` and should land first.

## Definition of done

- A measurement report with real numbers at several language counts, committed
  to `docs/reports/`.
- An explicit recommendation with the reasoning, including the two verification
  findings.
- Either the issue closed as "measured, no change warranted", or a follow-up
  issue opened describing the proposed divergence. **Not** a speculative
  implementation.
