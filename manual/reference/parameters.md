# Route parameter allowlists

Source provenance: the `*_PARAMS` declarations and `PER_FIELD_PARAMS` in
[`src/lib.rs`](../../src/lib.rs). `strict_params` consults these lists. **An
allowlisted name is not automatically a complete Solr implementation.** Status
means only the bounded behavior below; see [Compatibility](../../docs/COMPATIBILITY.md).

Status values: **implemented** = this bounded handler behavior is wired;
**constrained** = implemented only under the stated limit; **inert** = accepted
but intentionally has no effect; **warning-only** = accepted and warns;
**prefix-family** = the trailing-dot name admits a family of keys.

| Allowlist | Parameter | Status | Bounded behavior |
|---|---|---|---|
| `SELECT_PARAMS` | `q` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `df` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `fq` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `defType` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `qf` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `pf` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `mm` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `tie` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `boost` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `bq` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `bf` | implemented | Additive bounded function-query boost; invalid expressions return 400. |
| `SELECT_PARAMS` | `fl` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `rows` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `start` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.field` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.query` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.limit` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.mincount` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.sort` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.missing` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.range` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.range.start` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.range.end` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.range.gap` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.gridLevel` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.geom` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.maxCells` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.distErrPct` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.distErr` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `facet.heatmap.format` | constrained | Only `ints2D` is implemented; `png` is not. |
| `SELECT_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `json.facet` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `stats` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `stats.field` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `function` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.field` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.ngroups` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.limit` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.offset` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.sort` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.truncate` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `group.facet` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `sort` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `sfield` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `pt` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `d` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.fl` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.snippets` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.fragsize` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.simple.pre` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.simple.post` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.method` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.mergeContiguous` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.requireFieldMatch` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.preserveMulti` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `hl.fragmenter` | constrained | `gap` is effective; `regex` falls back to gap behavior. |
| `SELECT_PARAMS` | `hl.maxAnalyzedChars` | inert | Accepted but does not constrain Tantivy analysis yet. |
| `SELECT_PARAMS` | `hl.usePhraseHighlighter` | inert | Accepted but does not alter snippets yet. |
| `SELECT_PARAMS` | `hl.highlightMultiTerm` | inert | Accepted but does not expand multi-term highlights yet. |
| `SELECT_PARAMS` | `spellcheck` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `spellcheck.q` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `spellcheck.dictionary` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `spellcheck.collate` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
| `SELECT_PARAMS` | `TZ` | inert | Accepted and ignored while date math and calendar gaps are unsupported. |
| `PER_FIELD_PARAMS` | `facet.missing` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.limit` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.mincount` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.sort` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.range.start` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.range.end` | implemented | Implemented within the documented route boundary. |
| `PER_FIELD_PARAMS` | `facet.range.gap` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `commit` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `commitWithin` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `overwrite` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `softCommit` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `UPDATE_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `extractOnly` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `extractFormat` | constrained | Applies to `extractOnly`; accepted and ignored by indexing. |
| `EXTRACT_PARAMS` | `resource.name` | implemented | Filename/extension evidence for format detection after signatures and declared MIME, in both extract-only and indexing modes. |
| `EXTRACT_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `commit` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `commitWithin` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `softCommit` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `overwrite` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `uprefix` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `lowernames` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `captureAttr` | implemented | Implemented within the documented route boundary. |
| `EXTRACT_PARAMS` | `literal.` | prefix-family | Any `literal.<field>` key is accepted. |
| `EXTRACT_PARAMS` | `fmap.` | prefix-family | Any `fmap.<from>` key is accepted. |
| `PING_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `ADMIN_INFO_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `ADMIN_INFO_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `SCHEMA_FIELDTYPES_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `SCHEMA_FIELDTYPES_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `ADMIN_LUKE_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `ADMIN_LUKE_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `ADMIN_LUKE_PARAMS` | `numTerms` | inert | Accepted; no term histogram is produced. |
| `ADMIN_LUKE_PARAMS` | `show` | inert | Accepted; no response variant is selected. |
| `ADMIN_LUKE_PARAMS` | `fl` | inert | Accepted; no per-field selection is applied. |
| `MBEANS_PARAMS` | `stats` | implemented | Implemented within the documented route boundary. |
| `MBEANS_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `MBEANS_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `MBEANS_PARAMS` | `cat` | inert | Accepted; no bean filter is applied. |
| `MBEANS_PARAMS` | `key` | inert | Accepted; no bean filter is applied. |
| `MLT_PARAMS` | `q` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `df` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `fl` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `rows` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `start` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.fl` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.mintf` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.mindf` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.maxdf` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.minwl` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.maxwl` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.maxqt` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.maxntp` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.boost` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.interestingTerms` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `TZ` | inert | Accepted and ignored; date math and calendar gaps are unsupported. |
| `MLT_PARAMS` | `fq` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.match.include` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `mlt.match.offset` | implemented | Implemented within the documented route boundary. |
| `MLT_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `terms` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `terms.fl` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `terms.prefix` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `terms.limit` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `TERMS_PARAMS` | `json.nl` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest.buildAll` | inert | Accepted command echo; no dictionary build occurs. |
| `SUGGEST_PARAMS` | `suggest.build` | inert | Accepted command echo; no dictionary build occurs. |
| `SUGGEST_PARAMS` | `suggest.reload` | inert | Accepted command echo; no dictionary reload occurs. |
| `SUGGEST_PARAMS` | `suggest.dictionary` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest.count` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest.q` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest.cfq` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `suggest.highlight` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `wt` | implemented | Implemented within the documented route boundary. |
| `SUGGEST_PARAMS` | `omitHeader` | implemented | Implemented within the documented route boundary. |
