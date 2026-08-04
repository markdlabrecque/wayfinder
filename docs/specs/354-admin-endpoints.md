# #354 — three non-cloud endpoints from `search_api_solr_admin`

Branch: `354-admin-endpoints`. **Group C, lands last** — it owns the coverage
denominator and must recompute against the final endpoint set.

These come from `modules/search_api_solr_admin/`, a shipped submodule the
three-file snapshot did not include (now vendored by PREP-1). All three are on
the **standard, non-cloud connector**, so the SolrCloud non-goal does not excuse
them.

## Decisions already made

**1. `GET /<core>/analysis/field` — build it.**

`SolrFieldAnalysisForm.php:145-148` calls `getAnalysisQueryField()`
(`SolrConnectorPluginBase.php:971-973` → Solarium `createAnalysisField()`),
setting `analysis.fieldtype` and `analysis.fieldvalue`. It is an interactive AJAX
form: an operator types a value, picks a field type, and sees the analyzer
chain's token output.

Worth building beyond parity — it is exactly the "what did the analyzer do to my
text?" tool Wayfinder's own admin UI (v2.5) has no answer for, and Wayfinder has
a real analyzer chain to introspect.

**2. `GET /solr/admin/cores?action=RELOAD&core=<core>` — implement only if it is
needed.**

`StandardSolrConnector.php:24-40` (`reloadCore()`), driven by
`SolrReloadCoreForm` and by `SolrAdminCommandHelper.php:75` (Drush). Note the
path shape: **server-level, not core-relative** — it does not go in
`manifest.tsv` the way a core-relative GET does.

Determine whether Wayfinder has a reload concept at all. If a reload is
meaningless — nothing is cached that a reload would discard — **do not implement
a no-op that pretends otherwise.** Descope it with evidence, and record what
Wayfinder answers when a client calls it and why. If there *is* something a
reload would legitimately do, build that.

**3. `GET /<core>/admin/file?file=<name>` — documented divergence.**

`SolrConnectorPluginBase.php:1400-1412` (`getFile()`) returns raw configset
files. Wayfinder has no configset, so there is nothing to return. Record it in
PRD §5 with evidence rather than leaving it unexamined.

`uploadConfigset()` is Cloud-only (`StandardSolrCloudConnector.php:253-263`) and
stays out under the SolrCloud non-goal.

## Verify before implementing

With the submodule now vendored, confirm and report real line numbers for all
three call sites. The citations above come from the sweep, not from a read of
this tree.

Also establish, for `analysis/field`: what response shape Solr returns. It is a
nested structure of analyzer stages with per-stage token output, and it is the
part of this work most likely to be got wrong from memory. **Capture it.**

## Descopes need guards

Whatever is descoped (`admin/file`, and `admin/cores` if it goes that way) gets a
**self-expiring guard over the source channel**, matching
`tests/version_descope_guard.rs`. The guard asserts the reason still holds — that
the module still calls this, that Wayfinder still has no configset — so the entry
deletes itself when its premise expires instead of rotting into a permanently
green lie.

## The coverage denominator — you own it

The denominator currently holds 9 endpoints from the trace, and the trace could
not see any of these: the captured site had no `search_api_solr_admin`
interaction.

**Recomputing the coverage number against the widened denominator is part of this
work, not a follow-up.** See #225 for what the number is allowed to claim.

Sibling branches in this batch also add endpoints — #350 (POST routes), #351
(`/autocomplete`), #352 (`/suggest`). Each was told to report its addition in its
PR body and leave the number alone. **Collect those, land last, and recompute
once.** Read their merged PR bodies rather than trusting this list.

## Fixtures

`analysis/field` needs capture against real `solr:9` with a known field type and
input value, so the analyzer-stage output is ground truth rather than invention.
Append the block at the **end** of `solr-ref/capture.sh`, run with
`capture.sh --only <prefix>`, and commit fixtures before anything else.

`admin/cores` is server-level, not core-relative — if you capture it, it belongs
in `manifest-errors.tsv`, not `manifest.tsv`. The differential harness GETs every
`manifest.tsv` row verbatim as core-relative.

## Testing

Tests first, red, from fixtures. Cover the analyzer output shape per stage;
an unknown field type; an empty field value; the descope guards; and the
recomputed coverage assertion.

## Files

**You own:** `src/lib.rs` (routes), `src/coverage.rs` (**the denominator — yours
alone**), the analysis module, `src/schema.rs` if analyzer introspection needs an
accessor, PRD §5, `solr-ref/capture.sh` (append at end),
`solr-ref/manifest.tsv` / `manifest-errors.tsv`, the descope guard tests.

## Definition of done

- `analysis/field` served against captured fixtures, with real analyzer output.
- `admin/cores` either implemented or descoped with a stated answer and a guard.
- `admin/file` recorded as a documented divergence in PRD §5, with a guard.
- Coverage denominator recomputed once, against every endpoint this batch added,
  with the sources listed.
- Rust gates clean.
