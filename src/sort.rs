//! Parsing and validation for select and grouping sort specifications.

use crate::collector::{SortClause, SortKey};
use crate::error::WfError;
use crate::params::Params;
use crate::{function_query, schema};

/// Parses and validates the `sort` parameter, returning the clauses to order by
/// (empty means "no sort", i.e. Solr's default `score desc`).
///
/// Three hard 400s, all captured:
///
/// - Sorting on an undefined field.
/// - Sorting on a field that is not `fast`: a hard 400 in Solr (finding 11,
///   `err_bad_sort.json`), never a silent fallback.
/// - A clause whose direction token is missing or is not `asc`/`desc`
///   (`err_sort_no_direction.json`, `err_sort_bad_direction.json`).
///
/// The *order* of those checks is itself captured behaviour (finding 18), and it
/// has two independent halves, each with its own fixture — they are separate
/// claims and only one fixture each can establish them:
///
/// - **Across clauses:** left to right, stopping at the first bad clause, so one
///   bad clause rejects the whole spec rather than sorting on the valid prefix
///   (`err_sort_bad_clause_among_good.json`, and
///   `err_sort_field_before_direction.json` where an earlier clause's field error
///   beats a later clause's direction error).
/// - **Within a clause:** the direction is checked **before** the field is
///   resolved. Only `err_sort_direction_before_field.json` shows this —
///   `sort=body sideways` is bad in both ways at once, and Solr answers the
///   direction error. Every other captured spec is identical under either
///   within-clause order, so nothing else can be cited for it.
///
/// `score` is special-cased out of field resolution entirely. Note what
/// establishes what: `err_sort_score_bad_direction.json` shows only that `score`
/// is **not exempt from the direction check** — under direction-first, a bad
/// direction errors whether or not `score` is special-cased, so that fixture
/// says nothing about resolution. The special-casing itself is established by
/// `select_sort_score_{all,asc,desc}` returning 200 and ranking by score, which
/// an unresolvable field could not do.
///
/// The select workflow's `sort` param and grouping's `group.sort` (issue #290)
/// share this parser so both speak the
/// same field-direction grammar — comma does not delimit the field token,
/// direction is checked before the field resolves, and a dynamic-only match
/// sorts on its catch-all fast column (findings 18/34/35, issue #66).
pub(crate) fn parse_spec(
    schema: &crate::schema::WayfinderSchema,
    params: &Params,
    spec: &str,
) -> Result<Vec<SortClause>, WfError> {
    // Rewritten clause grammar (finding 34/35, issue #32). Scanned with an
    // absolute cursor into `spec` rather than `split(',')`: a comma does NOT
    // delimit the field token (`,id` is one token, which is exactly how the
    // leading/doubled-comma fixtures end up as *field* errors — the glued
    // token simply fails field resolution), and everything after the field
    // token up to the next comma (or end of spec) is the direction, checked
    // as a single trimmed chunk rather than split further — which is what
    // makes `sort=id asc garbage` a direction error instead of a silently
    // dropped extra token.
    let mut clauses = Vec::new();
    let mut pos = 0usize;
    loop {
        // Skip whitespace between clauses. Also the mechanism for "no more
        // clauses": an empty or all-whitespace spec, or a trailing comma
        // followed only by whitespace/end, lands here with nothing left.
        let ws = spec[pos..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(spec.len() - pos);
        pos += ws;
        if pos >= spec.len() {
            break;
        }

        // FIELD: the next whitespace-delimited token, starting at `pos`. A
        // comma does not delimit it.
        let field_len = spec[pos..]
            .find(char::is_whitespace)
            .unwrap_or(spec.len() - pos);
        let field_end = pos + field_len;
        let field_name = &spec[pos..field_end];

        // DIRECTION: from just past the field token to the next comma or end
        // of spec, trimmed, checked as one chunk against `asc`/`desc`.
        let dir_start = field_end;
        let comma_rel = spec[dir_start..].find(',');
        let dir_end = dir_start + comma_rel.unwrap_or(spec.len() - dir_start);
        let direction_raw = &spec[dir_start..dir_end];

        // Direction first, field second (finding 18/34).
        //
        // `pos` mirrors Solr's parser position — the *absolute* offset within
        // the whole spec just past this clause's field token, leading
        // whitespace included (finding 35): `pos=2` for `'id sideways'`
        // (`err_sort_bad_direction.json`), `pos=9` for the second clause of
        // `'id asc,id sideways'` (`err_sort_second_clause_bad_direction.json`),
        // `pos=15` — past the *whole* second field token — for
        // `'id asc,category'` (`err_sort_second_clause_no_direction.json`),
        // and `pos=4` with leading spaces preserved for `'  id sideways'`
        // (`err_sort_leading_whitespace.json`).
        let descending = match direction_raw.trim() {
            "asc" => false,
            "desc" => true,
            _ => {
                return Err(WfError::bad_request(
                    "wayfinder::BadSort",
                    format!(
                        "Can't determine a Sort Order (asc or desc) in sort spec '{spec}', pos={dir_start}"
                    ),
                )
                .with_params(params));
            }
        };

        let key = if field_name == "score" {
            SortKey::Score
        } else if field_name == "geodist()" {
            // #332: `sort=geodist() asc` ranks by ascending haversine distance.
            // The argless `geodist()` reads `sfield`/`pt` from the request
            // params (finding 133), exactly like `fl=dist:geodist()`; `sfield`
            // must name a declared `location` field. Direction-first still
            // holds (a bad direction 400s before this), matching `score`'s
            // special-casing.
            SortKey::Function(geodist_sort_func(schema, params)?)
        } else {
            // Resolved with the same static-before-dynamic precedence
            // indexing already uses (issue #66): a declared `[[fields]]`
            // entry wins over a `[[dynamic_fields]]` pattern that would also
            // match it, and a dynamic-only match sorts on the catch-all JSON
            // column it is actually indexed into (mirrors
            // `CoreIndex::rewrite_dynamic_fields`'s resolution for the query
            // path), not the bare field name.
            // #341/finding 186: a `date_range` field is a spatial field in
            // Solr's type hierarchy, and Solr refuses to sort on one with its
            // own message -- checked BEFORE the fast/docValues check, since the
            // refusal does not depend on whether the field has fast values
            // (the dynamic path's catch-all column does).
            if schema.resolved_value_kind(field_name) == Some(schema::ValueKind::DateRange) {
                return Err(WfError::bad_request(
                    "wayfinder::BadSort",
                    format!(
                        "Sorting not supported on SpatialField: {field_name}, instead try sorting by query."
                    ),
                )
                .with_params(params));
            }
            match schema.resolved_fast(field_name) {
                None => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!("can not sort on undefined field: {field_name}"),
                    )
                    .with_params(params));
                }
                Some(false) => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!(
                            "can not sort on a field w/o fast values (docValues): {field_name}"
                        ),
                    )
                    .with_params(params));
                }
                Some(true) => {
                    let column = schema
                        .resolved_fast_column(field_name)
                        .expect("resolved_fast confirmed this name resolves");
                    SortKey::Field(column)
                }
            }
        };

        // The schema's declared value kind travels with the clause so the
        // collector can materialise a segment-wide-absent column's missing
        // value as the right *type* (finding 36/37) — `score` has none, it is
        // never missing. `resolved_value_kind` already resolves any custom
        // `[[field_types]]`, which only ever produce `Text`, so there is no
        // numeric/date custom-type case this can miss. Resolved from the
        // original `field_name`, not the (possibly rewritten) column in
        // `key`, since a dynamic column's own name carries no schema entry.
        let value_kind = match &key {
            SortKey::Score | SortKey::Function(_) => None,
            SortKey::Field(_) => schema.resolved_value_kind(field_name),
        };
        clauses.push(SortClause::new(key, descending, value_kind));

        // Consume at most one comma after a valid clause. A trailing comma
        // (no more clauses after it) is fine; anything else — a second
        // consecutive comma, more text with no comma — starts the next loop
        // iteration as a new clause, whose field token then starts with a
        // comma and fails field resolution (the leading/doubled-comma cases).
        match comma_rel {
            Some(rel) => pos = dir_start + rel + 1,
            None => break,
        }
    }
    Ok(clauses)
}

/// Resolves the argless `geodist()` a `sort=geodist() ...` clause ranks by,
/// reading `sfield`/`pt` from the request params (finding 133) exactly as
/// `computed_fl_fields` does for `fl=dist:geodist()`. `sfield` must name a
/// declared `location` field; `pt` is `lat,lon`. The missing-/bad-param paths
/// carry no fixture (the capture always sends both), so they are the correct
/// 400 rather than a panic.
fn geodist_sort_func(
    schema: &schema::WayfinderSchema,
    params: &Params,
) -> Result<function_query::FuncQuery, WfError> {
    let sfield = params.get("sfield").ok_or_else(|| {
        WfError::bad_request(
            "wayfinder::BadSort",
            "geodist() sort requires the `sfield` request param".to_string(),
        )
    })?;
    if schema.location_fields(sfield).is_none() {
        return Err(WfError::bad_request(
            "wayfinder::BadSort",
            format!("geodist() sort sfield `{sfield}` is not a declared `location` field"),
        )
        .with_params(params));
    }
    let pt = params.get("pt").ok_or_else(|| {
        WfError::bad_request(
            "wayfinder::BadSort",
            "geodist() sort requires the `pt` request param".to_string(),
        )
    })?;
    let (lat_s, lon_s) = pt.split_once(',').ok_or_else(|| {
        WfError::bad_request(
            "wayfinder::BadSort",
            format!("geodist() sort pt `{pt}` is not a `lat,lon` point"),
        )
    })?;
    let lat: f64 = lat_s.trim().parse().map_err(|_| {
        WfError::bad_request(
            "wayfinder::BadSort",
            format!("geodist() sort pt `{pt}` has a non-numeric latitude"),
        )
    })?;
    let lon: f64 = lon_s.trim().parse().map_err(|_| {
        WfError::bad_request(
            "wayfinder::BadSort",
            format!("geodist() sort pt `{pt}` has a non-numeric longitude"),
        )
    })?;
    Ok(function_query::FuncQuery::GeoDist {
        sfield: sfield.to_string(),
        pt: (lat, lon),
    })
}
