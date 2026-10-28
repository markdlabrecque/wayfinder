//! Correctness gate for `wf-bench` (issue #440): does Wayfinder's answer
//! still make sense next to Solr's? Not exact parity. Scoring is allowed to
//! differ, so relevance-ranked results only need to mostly overlap. Counts,
//! fixed-sort order, facets, stats, and groups must agree.

use serde_json::Value;

/// Share of Solr's top results Wayfinder must also return for `ranked`.
pub const MIN_OVERLAP: f64 = 0.8;
const REL_TOL: f64 = 1e-9;

pub fn check(mode: &str, wf: &Value, solr: &Value) -> Result<(), String> {
    match mode {
        "ranked" => num_found(wf, solr).and_then(|()| overlap(wf, solr)),
        "exact" => num_found(wf, solr).and_then(|()| {
            let (w, s) = (ids(wf), ids(solr));
            (w == s)
                .then_some(())
                .ok_or_else(|| format!("order wf={w:?} solr={s:?}"))
        }),
        "facet" => num_found(wf, solr).and_then(|()| facets(wf, solr)),
        "stats" => num_found(wf, solr).and_then(|()| stats(wf, solr)),
        "group" => groups(wf, solr),
        "hl" => num_found(wf, solr)
            .and_then(|()| overlap(wf, solr))
            .and_then(|()| snippets(wf)),
        _ => Err(format!("unknown check mode {mode:?}")),
    }
}

fn num_found(wf: &Value, solr: &Value) -> Result<(), String> {
    let (w, s) = (&wf["response"]["numFound"], &solr["response"]["numFound"]);
    if w.is_null() || w != s {
        return Err(format!("numFound wf={w} solr={s}"));
    }
    Ok(())
}

fn ids(answer: &Value) -> Vec<String> {
    answer["response"]["docs"]
        .as_array()
        .map(|docs| docs.iter().map(|d| d["id"].to_string()).collect())
        .unwrap_or_default()
}

fn overlap(wf: &Value, solr: &Value) -> Result<(), String> {
    let (w, s) = (ids(wf), ids(solr));
    let hits = w.iter().filter(|id| s.contains(id)).count();
    let need = (s.len() as f64 * MIN_OVERLAP).ceil() as usize;
    (hits >= need)
        .then_some(())
        .ok_or_else(|| format!("top-{} overlap {hits}, need {need}", s.len()))
}

fn facets(wf: &Value, solr: &Value) -> Result<(), String> {
    let (w, s) = (&wf["facet_counts"], &solr["facet_counts"]);
    if w["facet_fields"] != s["facet_fields"] {
        return Err(format!(
            "facet_fields wf={} solr={}",
            w["facet_fields"], s["facet_fields"]
        ));
    }
    let ranges = s["facet_ranges"].as_object().into_iter().flatten();
    for (field, range) in ranges {
        let (rw, rs) = (&w["facet_ranges"][field]["counts"], &range["counts"]);
        if rw != rs {
            return Err(format!("facet_ranges.{field} wf={rw} solr={rs}"));
        }
    }
    Ok(())
}

fn close(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() <= REL_TOL * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

fn stats(wf: &Value, solr: &Value) -> Result<(), String> {
    let fields = solr["stats"]["stats_fields"].as_object();
    let Some(fields) = fields.filter(|f| !f.is_empty()) else {
        return Err("solr answer has no stats".into());
    };
    for (field, s) in fields {
        let w = &wf["stats"]["stats_fields"][field];
        for key in [
            "min",
            "max",
            "count",
            "missing",
            "sum",
            "sumOfSquares",
            "mean",
            "stddev",
        ] {
            if !close(&w[key], &s[key]) {
                return Err(format!("stats.{field}.{key} wf={} solr={}", w[key], s[key]));
            }
        }
    }
    Ok(())
}

fn group_counts(answer: &Value, field: &str) -> Vec<(String, String)> {
    let mut counts: Vec<_> = answer["grouped"][field]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            (
                g["groupValue"].to_string(),
                g["doclist"]["numFound"].to_string(),
            )
        })
        .collect();
    counts.sort();
    counts
}

fn groups(wf: &Value, solr: &Value) -> Result<(), String> {
    let fields = solr["grouped"].as_object();
    let Some(fields) = fields.filter(|f| !f.is_empty()) else {
        return Err("solr answer has no groups".into());
    };
    for (field, s) in fields {
        let w = &wf["grouped"][field];
        if w["matches"] != s["matches"] {
            return Err(format!(
                "grouped.{field}.matches wf={} solr={}",
                w["matches"], s["matches"]
            ));
        }
        let (gw, gs) = (group_counts(wf, field), group_counts(solr, field));
        if gw != gs {
            return Err(format!("grouped.{field} wf={gw:?} solr={gs:?}"));
        }
    }
    Ok(())
}

fn snippets(wf: &Value) -> Result<(), String> {
    for id in wf["response"]["docs"].as_array().into_iter().flatten() {
        let key = id["id"].as_str().unwrap_or_default();
        let has = wf["highlighting"][key].as_object().is_some_and(|fields| {
            fields
                .values()
                .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        });
        if !has {
            return Err(format!("no snippet for {key}"));
        }
    }
    Ok(())
}
