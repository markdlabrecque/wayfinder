//! Complete `/select` query workflow.
//!
//! The module owns parsing, planning, collection, post-processing, and wire
//! rendering. The HTTP handler only adapts Axum input into [`Params`] and the
//! returned JSON value into an Axum response.

use serde_json::{Map, Value, json};
use tantivy::query::{EmptyQuery, Occur, Query, QueryClone};

use crate::collector::SortClause;
use crate::core_index::CoreIndex;
use crate::error::WfError;
use crate::params::Params;
use crate::{
    AppState, facet, function_query, grouping, highlight, json_facet, query, schema, stats,
};

/// Maps a `CoreIndex::parse_query` failure to the right `WfError` shape:
/// finding 59's one 500 (a regex that parses as a query but fails automaton
/// compilation — `query::QueryError::RegexCompile`, carried through
/// `parse_query`'s `anyhow::Error` via `From`) gets the trace-carrying,
/// no-`metadata` envelope `err_regex_bad_class.json` pins; any other
/// unexpected failure (`QueryError::Internal` — e.g. a term-dictionary I/O
/// error) is a plain 500 with the ordinary `metadata` shape, not dressed up
/// in the regex one; every other failure (unknown field, bad syntax, an
/// unclosed regex, a prefix query on a numeric field) is an ordinary 400
/// `wayfinder::SyntaxError`, as before this issue.
fn query_parse_error(e: anyhow::Error, params: &Params) -> WfError {
    match e.downcast_ref::<query::QueryError>() {
        Some(query::QueryError::RegexCompile(_)) => {
            WfError::internal("wayfinder::RegexCompileError", e.to_string())
                .with_trace(e.to_string())
                .with_params(params)
        }
        Some(query::QueryError::Internal(_)) => {
            WfError::internal("wayfinder::QueryError", e.to_string()).with_params(params)
        }
        _ => WfError::bad_request("wayfinder::SyntaxError", e.to_string()).with_params(params),
    }
}

/// Returns raw Unicode-alphanumeric runs with both Rust byte ranges (for
/// slicing/collation) and Java UTF-16 code-unit offsets (for Solr's wire
/// `startOffset`/`endOffset`; `spellcheck_unicode_offsets.json`).
fn spellcheck_tokens(text: &str) -> Vec<(usize, usize, usize, usize, &str)> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut utf16_offset = 0;
    for (byte_offset, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            start.get_or_insert((byte_offset, utf16_offset));
        } else if let Some((byte_start, utf16_start)) = start.take() {
            tokens.push((
                byte_start,
                byte_offset,
                utf16_start,
                utf16_offset,
                &text[byte_start..byte_offset],
            ));
        }
        utf16_offset += ch.len_utf16();
    }
    if let Some((byte_start, utf16_start)) = start {
        tokens.push((
            byte_start,
            text.len(),
            utf16_start,
            utf16_offset,
            &text[byte_start..],
        ));
    }
    tokens
}

/// Builds the narrow issue-#223 spellcheck component from one real Tantivy
/// field dictionary. `dictionary` names `spellcheck_<dictionary>`, and the
/// first repeated request value wins through `Params::get`.
fn spellcheck(index: &CoreIndex, params: &Params) -> anyhow::Result<Value> {
    let map = params.get("json.nl") == Some("map");
    let empty = || {
        let named_list = || {
            if map {
                Value::Object(Map::new())
            } else {
                Value::Array(Vec::new())
            }
        };
        json!({ "suggestions": named_list(), "collations": named_list() })
    };

    let Some(dictionary) = params.get("spellcheck.dictionary") else {
        return Ok(empty());
    };
    let field_name = format!("spellcheck_{dictionary}");
    if !index.resolves_field_name(&field_name) {
        return Ok(empty());
    }
    let Some(text) = params.get("spellcheck.q") else {
        return Ok(empty());
    };
    let terms = index.field_terms(&field_name)?;
    let mut corrections = Vec::new();

    // ponytail: this is deliberately not Solr's configurable spellcheck
    // analyzer or ranking pipeline. It only scans raw Unicode-alphanumeric
    // query runs, matches exact dictionary terms verbatim, then chooses one
    // Damerau candidate at edit distance <= 2 (term-ascending tie break).
    // Extend it only with a captured analyzer/ranking contract.
    for (byte_start, byte_end, offset_start, offset_end, token) in spellcheck_tokens(text) {
        if terms.contains_key(token) {
            continue;
        }
        let candidate = terms
            .keys()
            .filter_map(|term| {
                let distance = query::levenshtein(token, term);
                (distance <= 2).then_some((distance, term))
            })
            .min_by(|(distance_a, term_a), (distance_b, term_b)| {
                distance_a.cmp(distance_b).then_with(|| term_a.cmp(term_b))
            })
            .map(|(_, term)| term.clone());
        if let Some(candidate) = candidate {
            corrections.push((
                byte_start,
                byte_end,
                offset_start,
                offset_end,
                token,
                candidate,
            ));
        }
    }

    let suggestions = if map {
        let mut suggestions = Map::new();
        for (_, _, start, end, token, candidate) in &corrections {
            suggestions.insert(
                (*token).to_string(),
                json!({
                    "numFound": 1,
                    "startOffset": start,
                    "endOffset": end,
                    "suggestion": [candidate],
                }),
            );
        }
        Value::Object(suggestions)
    } else {
        let mut suggestions = Vec::with_capacity(corrections.len() * 2);
        for (_, _, start, end, token, candidate) in &corrections {
            suggestions.push(json!(token));
            suggestions.push(json!({
                "numFound": 1,
                "startOffset": start,
                "endOffset": end,
                "suggestion": [candidate],
            }));
        }
        Value::Array(suggestions)
    };

    let collations = if params.bool_or("spellcheck.collate", false)? && !corrections.is_empty() {
        let mut corrected = String::with_capacity(text.len());
        let mut cursor = 0;
        for (byte_start, byte_end, _, _, _, candidate) in &corrections {
            corrected.push_str(&text[cursor..*byte_start]);
            corrected.push_str(candidate);
            cursor = *byte_end;
        }
        corrected.push_str(&text[cursor..]);
        if map {
            json!({ "collation": corrected })
        } else {
            json!(["collation", corrected])
        }
    } else if map {
        Value::Object(Map::new())
    } else {
        Value::Array(Vec::new())
    };

    Ok(json!({ "suggestions": suggestions, "collations": collations }))
}

/// Parses `fl` entries of the form `<alias>:geodist()` into computed fields,
/// resolving the argless `geodist()` against the `sfield`/`pt` request params
/// (the client-evidenced form, finding 133). Returns `(alias, FuncQuery)`
/// pairs the select workflow evaluates per doc and appends after the stored
/// fields -- Solr appends computed/transformer fields last (verified against
/// the `fl=*,dist:geodist()` capture). `sfield` must name a declared
/// `location` field (its two synthetic columns back the distance); every other
/// `fl` shape (literal name, `*`, `score`, `<alias>:<other-func>`) is not a
/// computed field and yields nothing here. (#331)
fn computed_fl_fields(
    fl: Option<&[String]>,
    params: &Params,
    schema: &schema::WayfinderSchema,
) -> Result<Vec<(String, function_query::FuncQuery)>, WfError> {
    let mut out = Vec::new();
    let Some(fl) = fl else {
        return Ok(out);
    };
    for entry in fl {
        let Some((alias, body)) = entry.split_once(':') else {
            continue;
        };
        let alias = alias.trim();
        let body = body.trim();
        if body != "geodist()" {
            // Only argless `geodist()` is supported in this tracer; an unknown
            // function or the explicit-args form is left for render_doc to treat
            // as a non-matching fl entry (no field, no output), matching Solr's
            // own handling of an unrecognised transformer name only loosely.
            continue;
        }
        if alias.is_empty() {
            return Err(WfError::bad_request(
                "wayfinder::SyntaxError",
                format!("fl entry `{entry}` needs an alias before `:geodist()`"),
            ));
        }
        let sfield = params.get("sfield").ok_or_else(|| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                "geodist() requires the `sfield` request param".to_string(),
            )
        })?;
        if schema.location_fields(sfield).is_none() {
            return Err(WfError::bad_request(
                "wayfinder::InvalidParam",
                format!("geodist() sfield `{sfield}` is not a declared `location` field"),
            ));
        }
        let pt = params.get("pt").ok_or_else(|| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                "geodist() requires the `pt` request param".to_string(),
            )
        })?;
        let (lat_s, lon_s) = pt.split_once(',').ok_or_else(|| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                format!("geodist() pt `{pt}` is not a `lat,lon` point"),
            )
        })?;
        let lat: f64 = lat_s.trim().parse().map_err(|_| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                format!("geodist() pt `{pt}` has a non-numeric latitude"),
            )
        })?;
        let lon: f64 = lon_s.trim().parse().map_err(|_| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                format!("geodist() pt `{pt}` has a non-numeric longitude"),
            )
        })?;
        out.push((
            alias.to_string(),
            function_query::FuncQuery::GeoDist {
                sfield: sfield.to_string(),
                pt: (lat, lon),
            },
        ));
    }
    Ok(out)
}

/// Runs one validated select request through the complete query workflow.
///
/// The caller owns transport concerns: request decoding, core routing, and the
/// route parameter allowlist. All select phase ordering and error timing live
/// behind this interface.
pub(super) fn execute(state: &AppState, params: Params) -> Result<Value, WfError> {
    let sort: Vec<SortClause> = match params.get("sort") {
        None => Vec::new(),
        Some(spec) => crate::sort::parse_spec(&state.index.wf_schema, &params, spec)?,
    };

    // Read *before* the base query runs, matching Solr's own timing: an
    // invalid value here answers with the error-only envelope, no `response`
    // block (`bool_facet_invalid.json`) -- unlike `facet.missing`, which
    // `facet::facet_counts` reads after the query and whose error therefore
    // carries one. `omitHeader` is not read here: the adapter's `check_params`
    // call already validated it (issue #214).
    let facet_requested = params.bool_or("facet", false)?;
    let stats_requested = params.bool_or("stats", false)?;
    let hl_requested = params.bool_or("hl", false)?;
    // Use the same strict Solr boolean parser as `hl`: only true enables the
    // component; false and an absent param leave its key out of the envelope.
    let spellcheck_requested = params.bool_or("spellcheck", false)?;

    // `bf` and `boost` no longer need the #232 accept-and-warn treatment:
    // function queries are implemented (issue #289), so a function-form
    // `boost` and any `bf` are applied rather than ignored. `select_warnings`
    // still collects facet warnings below.
    let mut select_warnings = Vec::new();

    let default_field = params
        .get("df")
        .unwrap_or(&state.index.wf_schema.core.default_field)
        .to_string();

    // No `q` matches nothing — it does *not* default to `*:*`. Solr answers 200
    // with an empty result set (`err_missing_q.json`), which resolves
    // tracer-bullet review follow-up 2 against the fixture.
    let parsed = match params.get("q") {
        None => None,
        Some(q) => {
            // `defType=edismax` (issue #7, PRD §5 v1 exception) switches only
            // `q`'s own parser to the dismax-style qf/pf/mm/tie/boost/bq
            // composition (`CoreIndex::parse_edismax_query`) — `fq` below is
            // untouched, always the plain Solr query grammar, matching real
            // Solr (`defType` only ever governs `q`). A `q` beginning with a
            // function-query local-params block (`{!func}` / `{!boost b=}`)
            // takes precedence over `defType` — those are query parsers in
            // their own right (issue #289).
            let query = if let Some(func_q) = state
                .index
                .parse_function_query_q(q, &default_field, Some(&params))
                .map_err(|e| query_parse_error(anyhow::Error::from(e), &params))?
            {
                func_q
            } else if params.get("defType") == Some("edismax") {
                let qf = params.get("qf").unwrap_or("");
                let pf = params.get("pf");
                let mm = params.get("mm");
                let tie: f32 = params
                    .get("tie")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let bq: Vec<String> = params
                    .get_all("bq")
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                // `boost` and `bf` are function-query params (issue #289): a
                // plain number like `boost=2` is Solr's simplest constant
                // function, a function form like `boost=product(rating,2)` or
                // any `bf` is evaluated per document. Both are passed raw to
                // `parse_edismax_query`, which applies them.
                let boost = params.get("boost").map(str::to_string);
                let bf: Vec<String> = params
                    .get_all("bf")
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                state
                    .index
                    .parse_edismax_query(
                        q,
                        &default_field,
                        qf,
                        pf,
                        mm,
                        tie,
                        &bq,
                        boost.as_deref(),
                        &bf,
                    )
                    .map_err(|e| query_parse_error(anyhow::Error::from(e), &params))?
            } else {
                state
                    .index
                    .parse_query(q, &default_field)
                    .map_err(|e| query_parse_error(e, &params))?
            };

            let mut filter_queries = Vec::new();
            for fq in params.get_all("fq") {
                // `{!geofilt}`/`{!bbox}`/`{!func}`/`{!frange}`/`{!boost}` are
                // position-0 query parsers in their own right (#289/#333/#332);
                // like the `q` path above, try the function-query dispatcher
                // (with request params, which the geo filters need) before the
                // plain grammar. `parse_query` re-runs the same dispatcher with
                // `None`, so a non-geo block costs one redundant `parse_block`
                // here and nothing more.
                let parsed = if let Some(g) = state
                    .index
                    .parse_function_query_q(fq, &default_field, Some(&params))
                    .map_err(|e| query_parse_error(anyhow::Error::from(e), &params))?
                {
                    g
                } else {
                    state
                        .index
                        .parse_query(fq, &default_field)
                        .map_err(|e| query_parse_error(e, &params))?
                };
                filter_queries.push(parsed);
            }
            Some((query, filter_queries))
        }
    };

    let start: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // `rows_limit` is a Wayfinder cap with no Solr equivalent, so an
    // over-limit request is clamped rather than rejected — a clamp keeps a
    // client that asks for too much working, a 400 breaks it.
    let rows: usize = params
        .get("rows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(state.config.query.rows_limit);

    // Result grouping (issue #290, PRD §5 v3): `group=true` swaps the
    // `response` doclist for a `grouped` envelope keyed by `group.field`. The
    // collector buckets every match by the group field's fast value (a
    // single-valued, non-text field — validated inside `grouping::grouping`,
    // which 400s on undefined / non-fast / multiValued the way Solr does,
    // finding 130). Running here, before the ungrouped top-N search, means a
    // grouped request never materialises the hits it would then discard: the
    // whole ungrouped middle section below is skipped, and everything after it
    // (`facet_counts`, `stats`, `highlighting`, `spellcheck`, the header) is
    // shared — a grouped response carries those blocks exactly as an ungrouped
    // one does (issue #338, findings 160/161/162).
    //
    // `fl`/`wants_score` are derived the same way the ungrouped path derives
    // them below; duplicated locally so this call is self-contained and leaves
    // that path byte-identical.
    let fl_group: Option<Vec<String>> = params
        .get("fl")
        .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());
    let wants_score_group = fl_group
        .as_deref()
        .is_some_and(|fl| fl.iter().any(|f| f == "score"));
    let grouped = grouping::grouping(
        &state.index,
        &params,
        parsed.as_ref().map(|(q, fqs)| (q.as_ref(), fqs.as_slice())),
        &sort,
        rows,
        start,
        fl_group.as_deref(),
        wants_score_group,
    )?;

    // Fused faceting (issue #246): the `facet.field` terms aggregation runs
    // over exactly the doc set the hit list iterates (`q` AND every `fq`), so
    // planning it here lets the main search compute both in one pass instead
    // of walking the same postings twice — ~5 ms of the 6.6 ms faceting cost
    // at 2M docs was that second walk.
    //
    // A planning error is *discarded*, not reported: the request then takes
    // today's unfused path, which re-derives the identical error at its
    // original point in the request lifecycle, so its message, its
    // `PreQueryFacetError` treatment and whether the envelope carries a
    // `response` block all stay bit-identical. Double validation costs nothing
    // on a request that is already failing.
    let mut facet_field_plan: Option<facet::FacetFieldsPlan> = None;
    let mut facet_field_aggs = None;
    // A grouped request has no ungrouped hit list and no `response` block at
    // all: `grouped` stands where `response` would, and `highlighting` covers
    // the documents the doclists rendered instead of `response.docs` (issue
    // #338). Skipping this whole section is what keeps the property that a
    // grouped request never materialises the hits it would then discard.
    let (response, page) = match &grouped {
        Some(outcome) => (None, outcome.rendered.clone()),
        None => {
            facet_field_plan = if facet_requested {
                facet::plan_facet_fields(&state.index, &params)
                    .ok()
                    .filter(|plan| !plan.fields.is_empty())
            } else {
                None
            };
            // #295: an excluded facet.field (`{!ex=...}`) counts against a reduced
            // filter set, which the fused aggregation (over the full q+fq set) cannot
            // produce. Drop the plan so the dispatch below takes the unfused path,
            // which builds a per-facet base. Multi-select facet requests are rare and
            // not the hot path, so forgoing fusing for them is a deliberate
            // simplification (see `FacetFieldsPlan::exclusion_active`).
            if facet_field_plan
                .as_ref()
                .is_some_and(|plan| plan.exclusion_active)
            {
                facet_field_plan = None;
            }

            // Bounded search (issue #242): only the first `start + rows` hits are
            // materialised; `num_found` and `max_score` still cover every match.
            let outcome = match &parsed {
                None => crate::collector::TopOutcome {
                    num_found: 0,
                    max_score: None,
                    top: Vec::new(),
                },
                Some((query, filter_queries)) => {
                    let unfused = || {
                        state
                            .index
                            .search_top(
                                query.as_ref(),
                                filter_queries,
                                &sort,
                                start.saturating_add(rows),
                            )
                            .map_err(|e| {
                                WfError::internal("wayfinder::SearchError", e.to_string())
                                    .with_params(&params)
                            })
                    };
                    // Attempted, not borrowed-through, so the plan can be dropped
                    // below without fighting the borrow checker.
                    let attempt = facet_field_plan.as_ref().map(|plan| {
                        state.index.search_top_with_aggs(
                            query.as_ref(),
                            filter_queries,
                            &sort,
                            start.saturating_add(rows),
                            plan.aggregations.clone(),
                        )
                    });
                    match attempt {
                        Some(Ok((outcome, aggs))) => {
                            facet_field_aggs = Some(aggs);
                            outcome
                        }
                        // An aggregation-class refusal (bucket limit, memory limit,
                        // a malformed aggregation) is exactly the error the unfused
                        // path raises out of `facet_counts` as a 400
                        // `wayfinder::FacetError` with the `response` block attached
                        // -- not the 500 `wayfinder::SearchError` it would become
                        // here. Un-fuse and let that path answer it, so the wire
                        // output stays bit-identical whichever way the request went.
                        Some(Err(e)) if crate::core_index::is_aggregation_error(&e) => {
                            facet_field_plan = None;
                            unfused()?
                        }
                        Some(Err(e)) => {
                            return Err(WfError::internal("wayfinder::SearchError", e.to_string())
                                .with_params(&params));
                        }
                        None => unfused()?,
                    }
                }
            };

            let num_found = outcome.num_found;

            let fl: Option<Vec<String>> = params
                .get("fl")
                .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());

            let page = outcome
                .top
                .iter()
                .skip(start)
                .take(rows)
                .copied()
                .collect::<Vec<_>>();

            // `fl=score` is what turns scoring output on at all (Solr), so this is
            // the single check that gates both the per-doc `score` key and
            // `response.maxScore` below.
            let wants_score = fl
                .as_deref()
                .is_some_and(|fl| fl.iter().any(|f| f == "score"));

            // #331: `<alias>:geodist()` fl entries are computed fields, resolved from
            // the `sfield`/`pt` request params and evaluated per doc below. Built once
            // here so the page loop only does the per-doc fast-field read.
            let computed = computed_fl_fields(fl.as_deref(), &params, &state.index.wf_schema)?;

            let mut docs = Vec::with_capacity(page.len());
            for (score, addr) in page.iter().copied() {
                let mut doc = state
                    .index
                    .render_doc(addr, fl.as_deref(), Some(score))
                    .map_err(|e| {
                        WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
                    })?;
                // Computed fields append after the stored fields (Solr's transformer
                // ordering; the `fl=*,dist:geodist()` capture places `dist` last).
                for (alias, func) in &computed {
                    let value = state.index.eval_function(addr, func).map_err(|e| {
                        WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
                    })?;
                    if let Value::Object(map) = &mut doc {
                        map.insert(alias.clone(), json!(value));
                    }
                }
                docs.push(doc);
            }

            // Key order in the fixtures is `numFound, start, maxScore, numFoundExact,
            // docs` — `maxScore` sits between `start` and `numFoundExact`, which a
            // `json!` object literal can't express conditionally, so this is built
            // the same way `response_header` is below. Built *before* `facet_result`
            // (issue #35): a `facet.query`/`facet.field` error is detected only after
            // the base query has already run, so Solr's own fixtures for those errors
            // (`facet_unknown_field.json`, `facet_err_query_single.json`) still carry
            // this `response` block alongside `error` — it has to exist already so it
            // can be attached to that error below.
            let mut response = Map::new();
            response.insert("numFound".to_string(), json!(num_found));
            response.insert("start".to_string(), json!(start));
            if wants_score {
                // ponytail: computed as the max score across the *whole*
                // (unpaginated) match set, not just the current page — an
                // unverified choice, not a fixtured fact. Every scored fixture
                // (`select_term_scored.json`, `select_quick_scored.json`) has
                // `start=0` with the full result set on one page, so page-max and
                // global-max are indistinguishable there; no fixture pages a scored
                // query to tell them apart.
                //
                // Zero hits with `score` in `fl` still carries the key: Solr
                // reports `"maxScore":0.0` alongside `numFound: 0` and
                // `docs: []` rather than omitting it. Evidence is a single
                // fixture, `pls_unmatched.json` (issue #340) — but it is the
                // only one there is, being the only zero-hit capture with
                // `score` in `fl` (every other empty response, `facet_zero`
                // through `mlt_no_interesting_terms`, omits `maxScore` because
                // it never asked for a score).
                response.insert(
                    "maxScore".to_string(),
                    json!(outcome.max_score.unwrap_or(0.0)),
                );
            }
            response.insert("numFoundExact".to_string(), json!(true));
            response.insert("docs".to_string(), json!(docs));
            (Some(response), page)
        }
    };

    // Facet and stats counts are both aggregated over a *real* query (`q` AND
    // every `fq`), not over `hits`: Solr enumerates the field's whole term
    // dictionary / metric aggregation over the matching set, which the hit
    // list cannot see (`search` filters post-hoc with `retain`, so the
    // Boolean query is rebuilt here rather than reused). Shared between both
    // features rather than built twice.
    let mut base: facet::BaseClauses = match &parsed {
        // No `q` matches nothing, so neither does any facet/stats block — but
        // the term dictionary is still enumerated, at 0, exactly as
        // `facet_zero` shows for a `q` that matches nothing, and `stats_zero`
        // shows for stats.
        None => vec![(Occur::Must, Box::new(EmptyQuery) as Box<dyn Query>)],
        Some((query, filter_queries)) => std::iter::once((Occur::Must, query.box_clone()))
            .chain(
                filter_queries
                    .iter()
                    .map(|fq| (Occur::Must, fq.box_clone())),
            )
            .collect(),
    };
    // `group.truncate=true` (issue #338, finding 161): facets, `stats`,
    // `facet.query` and `facet.range` are all computed over the *collapsed*
    // group set rather than every matching document, so the restriction goes
    // into the one base both components share — one place, and `stats` follows
    // for free (`g338_truncate_stats`), which the ticket's facets-only premise
    // missed. It is appended *after* the `fq` clauses so #295's positional
    // `{!tag}`/`{!ex}` alignment is untouched, and (having no tag) no
    // `{!ex=...}` can drop it.
    if let Some(query) = grouped.as_ref().and_then(|g| g.truncate_query.as_ref()) {
        base.push((Occur::Must, Box::new(query.clone()) as Box<dyn Query>));
    }
    let group_facet = grouped.as_ref().and_then(|g| g.group_facet.as_ref());
    // ponytail: the ungrouped component code attaches the already-built
    // `response` block to a facet/stats/hl 400 (issue #35's precedent). A
    // grouped response has no `response` block to attach, and no fixture
    // captures a grouped request with an invalid facet/stats/hl param, so the
    // grouped path answers with the error envelope alone rather than inventing
    // a shape. Capture one before relying on it either way.
    let attach_response = |err: WfError| match &response {
        Some(response) => err.with_response(Value::Object(response.clone())),
        None => err,
    };

    // `facet=true` gates the whole block; `facet.field` alone does not turn
    // faceting on and the key stays absent (findings fact 4). Computed *before*
    // `responseHeader` is built, not after: Solr's own `responseHeader` key
    // order is `warnings, status, QTime, params` — `warnings` leads, it does
    // not trail (finding 29 / issue #24) — and `serde_json`'s `preserve_order`
    // feature (issue #25) means the order keys are inserted in is now the order
    // they are emitted in, so `warnings` has to be known before the object
    // literal is written.
    let facet_result = if facet_requested {
        // Issue #246: when the plan phase succeeded above, `facet.field`'s
        // buckets are already computed and only need shaping; otherwise this
        // is the unchanged, unfused path.
        let counts = match (group_facet, &facet_field_plan, &facet_field_aggs) {
            // `group.facet=true` (issue #338) counts distinct groups, not
            // documents, which the fused document-count aggregation cannot
            // produce — and never coexists with a plan anyway, since the
            // grouped path skips the planning phase entirely.
            (Some(group), _, _) => facet::facet_counts_grouped(
                &state.index,
                &state.config,
                &params,
                &default_field,
                &base,
                group,
            ),
            (None, Some(plan), Some(aggs)) => facet::facet_counts_fused(
                &state.index,
                &state.config,
                &params,
                &default_field,
                &base,
                (plan, aggs),
            ),
            _ => facet::facet_counts(&state.index, &state.config, &params, &default_field, &base),
        };
        Some(counts.map_err(|e| {
            // Issue #35: `facet.range` is detected before the base
            // query ever runs (Solr's own `facet_err_range_single.json`
            // has no `response` block), while `facet.query`/
            // `facet.field` errors are detected after it (Solr's
            // `facet_unknown_field.json` / `facet_err_query_single.json`
            // do). `facet::facet_counts` marks the former with
            // `PreQueryFacetError` so only the latter gets `response`
            // attached here.
            let err =
                WfError::bad_request("wayfinder::FacetError", e.to_string()).with_params(&params);
            if e.downcast_ref::<facet::PreQueryFacetError>().is_some() {
                err
            } else {
                attach_response(err)
            }
        })?)
    } else {
        None
    };
    // Select-level warnings precede facet warnings because they describe
    // request parameters ignored before facet processing begins.
    if let Some((_, facet_warnings)) = &facet_result {
        select_warnings.extend(facet_warnings.iter().cloned());
    }

    // `json.facet` (issue #343): self-gating on the param's presence — there is
    // no `json.facet=true` switch, and `facet=true` is not sent alongside it, so
    // it neither reads nor is read by `facet_requested`. It shares `base`, so
    // its implicit `count` and every bucket count track `q` and `fq`.
    //
    // The error split is `facet_counts`' exactly: a `json.facet` *parse*
    // failure is detectable before the base query runs, so Solr's own fixtures
    // for it (`jf343_err_bad_json.json`, `jf343_err_bad_type.json`) carry no
    // `response` block, while a field-resolution failure
    // (`jf343_err_unknown_field.json`) does — `json_facet::json_facets` marks
    // the former with the same `facet::PreQueryFacetError`.
    let json_facet_result = json_facet::json_facets(&state.index, &params, &base).map_err(|e| {
        let err =
            WfError::bad_request("wayfinder::JsonFacetError", e.to_string()).with_params(&params);
        if e.downcast_ref::<facet::PreQueryFacetError>().is_some() {
            err
        } else {
            attach_response(err)
        }
    })?;

    // `stats=true` gates the whole `stats` block the same way `facet=true`
    // gates `facet_counts` — `stats.field` alone does not turn it on (mirrors
    // `facet.field`'s own convention, and matches `stats_key_absent_without_stats_true`).
    let stats_result = if stats_requested {
        // Deliberately NOT group-aware: `group.facet=true` leaves `stats`
        // reporting the full, ungrouped figures (`g338_groupfacet_stats`),
        // unlike `group.truncate`, which reaches `stats` through `base` above.
        Some(stats::stats(&state.index, &params, &base).map_err(|e| {
            let err =
                WfError::bad_request("wayfinder::StatsError", e.to_string()).with_params(&params);
            // Same pre-/post-query split `facet_counts` makes just above: a
            // refusal Solr raises before the base query runs carries no
            // `response` block (`dr341_err_stats`, #341).
            if e.downcast_ref::<stats::PreQueryStatsError>().is_some() {
                err
            } else {
                attach_response(err)
            }
        })?)
    } else {
        None
    };

    // `hl=true` gates the whole `highlighting` block (finding 52); it is
    // keyed by unique-key value over the docs actually returned on this
    // page, matching `response.docs`'s own pagination — or, on the grouped
    // path, the union of every rendered doclist (`page` is
    // `GroupedOutcome::rendered` there, `g338_hl`).
    let highlighting_result = if hl_requested {
        let result = match &parsed {
            Some((query, _)) => highlight::highlighting(
                &state.index,
                &params,
                &default_field,
                query.as_ref(),
                &page,
                &state.index.wf_schema.core.unique_key,
            ),
            // No `q` matches nothing, so `page` is always empty here too —
            // an empty `highlighting` object, not an absent key.
            None => Ok(Value::Object(Map::new())),
        }
        .map_err(|e| {
            // An undefined/non-text `hl.fl` field is a request-input
            // problem (`highlight::InvalidHlField`), rendered as Solr's own
            // 400 -- mirroring `facet.field`'s own unknown-field handling
            // just above, including carrying the base query's already-built
            // `response` block alongside `error` (issue #35's precedent;
            // unfixtured for `hl.fl` specifically, since no captured
            // `hl_*` fixture exercises an invalid field, but the shape
            // should be consistent with `facet_unknown_field.json`'s). A
            // failure that isn't request-input (a genuine Tantivy/searcher
            // error) stays a 500.
            if e.downcast_ref::<highlight::InvalidHlField>().is_some() {
                attach_response(
                    WfError::bad_request("wayfinder::HighlightError", e.to_string())
                        .with_params(&params),
                )
            } else {
                WfError::internal("wayfinder::HighlightError", e.to_string()).with_params(&params)
            }
        })?;
        Some(result)
    } else {
        None
    };

    // `responseHeader.warnings` is absent unless there is something to warn
    // about (every fixture that isn't a Points-based `facet.field` at
    // mincount 0 lacks the key) — never an empty array — and, when present,
    // leads the object rather than trailing it.
    let mut response_header = Map::new();
    if !select_warnings.is_empty() {
        response_header.insert("warnings".to_string(), json!(select_warnings));
    }
    response_header.insert("status".to_string(), json!(0));
    response_header.insert("QTime".to_string(), json!(0));
    response_header.insert("params".to_string(), json!(params.echo()));

    // `omitHeader=true` drops the header and nothing else (issue #143); the
    // `response`/`grouped` block and every optional block below are
    // unaffected. See `Params::omit_header` for the ground truth and the
    // error-path ceiling.
    //
    // A grouped response emits `grouped` where an ungrouped one emits
    // `response`, in the same slot — hence the fixtures' top-level key order
    // `responseHeader, grouped, facet_counts, stats, highlighting` (issue
    // #338, `g338_all`).
    let mut root = Map::new();
    if !params.omit_header() {
        root.insert("responseHeader".to_string(), Value::Object(response_header));
    }
    match grouped {
        Some(outcome) => {
            root.insert("grouped".to_string(), outcome.block);
        }
        None => {
            root.insert(
                "response".to_string(),
                Value::Object(response.expect("the ungrouped path always builds a response block")),
            );
        }
    }
    let mut body = Value::Object(root);

    if let Some((facet_counts, _)) = facet_result {
        body["facet_counts"] = facet_counts;
    }
    // `facets` sits between `facet_counts` and `stats`
    // (`jf343_with_classic_stats.json`'s top-level order), and `serde_json`'s
    // `preserve_order` makes this insertion order the wire order — so the slot
    // is these three lines' position, nothing else (issue #343).
    if let Some(facets) = json_facet_result {
        body["facets"] = facets;
    }
    if let Some(stats) = stats_result {
        body["stats"] = stats;
    }

    if let Some(highlighting) = highlighting_result {
        body["highlighting"] = highlighting;
    }
    if spellcheck_requested {
        body["spellcheck"] = spellcheck(&state.index, &params).map_err(|e| {
            WfError::internal("wayfinder::SpellcheckError", e.to_string()).with_params(&params)
        })?;
    }

    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{ServerConfig, build};

    #[test]
    fn plain_select_runs_through_the_workflow_interface() {
        let root = tempdir().expect("temp dir");
        let schema_path = root.path().join("schema.toml");
        let data_dir = root.path().join("data");
        fs::write(
            &schema_path,
            r#"
[core]
name = "workflow"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_general"
stored = true
"#,
        )
        .expect("schema");
        let server = build(&schema_path, &data_dir, ServerConfig::default()).expect("app state");
        let params = Params::parse("q=*:*&fl=id&wt=json").allow_omit_header();

        let body = execute(&server.shutdown.0, params).expect("plain select");

        assert_eq!(
            body,
            json!({
                "responseHeader": {
                    "status": 0,
                    "QTime": 0,
                    "params": {"q": "*:*", "fl": "id", "wt": "json"}
                },
                "response": {
                    "numFound": 0,
                    "start": 0,
                    "numFoundExact": true,
                    "docs": []
                }
            })
        );
    }
}
