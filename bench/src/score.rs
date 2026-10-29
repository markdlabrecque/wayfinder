//! The one score the tuning loop chases (issue #452). Every metric becomes a
//! Wayfinder/Solr ratio, inverted for higher-is-better metrics so below 1.0
//! always means Wayfinder wins. The score is the weighted geometric mean of
//! those ratios, with the weights read from `bench/weights.tsv`.

use serde_json::Value;

/// Metric names and their weights, in file order.
pub type Weights = Vec<(String, f64)>;

/// Parses `metric<TAB>weight` lines. `#` comments and blank lines are
/// skipped. Weights must be positive numbers and each metric listed once.
pub fn parse_weights(text: &str) -> Result<Weights, String> {
    let mut weights: Weights = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| format!("weights line {}: {why}: {line:?}", i + 1);
        let (name, weight) = line
            .split_once('\t')
            .ok_or_else(|| bad("expected metric<TAB>weight"))?;
        let name = name.trim();
        let weight: f64 = weight
            .trim()
            .parse()
            .map_err(|_| bad("weight is not a number"))?;
        if !(weight.is_finite() && weight > 0.0) {
            return Err(bad("weight must be above zero"));
        }
        if weights.iter().any(|(n, _)| n == name) {
            return Err(bad("metric listed twice"));
        }
        weights.push((name.to_string(), weight));
    }
    Ok(weights)
}

/// Wayfinder over Solr for a lower-is-better metric, Solr over Wayfinder for
/// a higher-is-better one, so below 1.0 means Wayfinder wins either way. A
/// zero or non-finite measurement is an error rather than a NaN or infinite
/// ratio that would wreck the score.
pub fn ratio(wf: f64, solr: f64, higher_is_better: bool) -> Result<f64, String> {
    for (engine, v) in [("wayfinder", wf), ("solr", solr)] {
        if !(v.is_finite() && v > 0.0) {
            return Err(format!("{engine} measured {v}, need a positive number"));
        }
    }
    Ok(if higher_is_better {
        solr / wf
    } else {
        wf / solr
    })
}

/// Every scored metric as a Wayfinder/Solr ratio, named as in
/// `bench/weights.tsv`, from the two engines' result JSON. Metrics measured
/// per query shape or load level give one ratio each, paired by position; the
/// score folds them into one per metric. A section missing from either engine
/// or a row count that differs is an error, so no row is silently dropped.
pub fn ratios(wf: &Value, solr: &Value) -> Result<Vec<(&'static str, f64)>, String> {
    fn push(
        out: &mut Vec<(&'static str, f64)>,
        name: &'static str,
        w: &Value,
        s: &Value,
        higher_is_better: bool,
    ) -> Result<(), String> {
        let num = |v: &Value, engine: &str| {
            v.as_f64()
                .ok_or_else(|| format!("{name}: {engine} has no number"))
        };
        let r = ratio(num(w, "wayfinder")?, num(s, "solr")?, higher_is_better)
            .map_err(|e| format!("{name}: {e}"))?;
        out.push((name, r));
        Ok(())
    }
    fn rows<'a>(
        wf: &'a Value,
        solr: &'a Value,
        k: &str,
    ) -> Result<Vec<(&'a Value, &'a Value)>, String> {
        let get = |v: &'a Value, engine: &str| {
            v[k].as_array()
                .ok_or_else(|| format!("{k}: {engine} has no rows"))
        };
        let (rw, rs) = (get(wf, "wayfinder")?, get(solr, "solr")?);
        if rw.len() != rs.len() {
            return Err(format!(
                "{k}: wayfinder has {} rows, solr has {}",
                rw.len(),
                rs.len()
            ));
        }
        Ok(rw.iter().zip(rs).collect())
    }
    let mut out = Vec::new();
    for (qw, qs) in rows(wf, solr, "queries")? {
        for (name, key) in [
            ("p50", "p50"),
            ("p95", "p95"),
            ("p99", "p99"),
            ("cpu_ms_per_1k", "cpu_ms_per_1k"),
            ("repeat_p50", "repeat_p50"),
            ("first_ms", "cold_ms"),
        ] {
            push(&mut out, name, &qw[key], &qs[key], false)?;
        }
    }
    for (cw, cs) in rows(wf, solr, "cold")? {
        for (name, key) in [
            ("cold_p50", "p50"),
            ("cold_p95", "p95"),
            ("cold_p99", "p99"),
        ] {
            push(&mut out, name, &cw[key], &cs[key], false)?;
        }
    }
    for (lw, ls) in rows(wf, solr, "load")? {
        push(&mut out, "load_rps", &lw["rps"], &ls["rps"], true)?;
        for (name, key) in [
            ("load_p50", "p50"),
            ("load_p95", "p95"),
            ("load_p99", "p99"),
        ] {
            push(&mut out, name, &lw[key], &ls[key], false)?;
        }
    }
    for name in [
        "startup_ms",
        "index_size_mb",
        "mem_idle_mb",
        "mem_post_index_mb",
        "mem_peak_mb",
    ] {
        push(&mut out, name, &wf[name], &solr[name], false)?;
    }
    push(
        &mut out,
        "index_docs_per_s",
        &wf["index_docs_per_s"],
        &solr["index_docs_per_s"],
        true,
    )?;
    let (fw, fs) = (&wf["add_to_searchable"], &solr["add_to_searchable"]);
    push(
        &mut out,
        "add_to_searchable_p50_ms",
        &fw["p50_ms"],
        &fs["p50_ms"],
        false,
    )?;
    push(
        &mut out,
        "add_to_searchable_p95_ms",
        &fw["p95_ms"],
        &fs["p95_ms"],
        false,
    )?;
    Ok(out)
}

/// One ratio per metric, in order of first appearance: the plain geometric
/// mean of that metric's ratios (one per query shape, load level, ...), so a
/// metric measured many times counts no more than one measured once.
pub fn metric_ratios(ratios: &[(&str, f64)]) -> Result<Vec<(String, f64)>, String> {
    let mut sums: Vec<(String, f64, usize)> = Vec::new();
    for &(name, r) in ratios {
        if !(r.is_finite() && r > 0.0) {
            return Err(format!("{name}: ratio {r} is not a positive number"));
        }
        match sums.iter_mut().find(|(n, _, _)| n == name) {
            Some((_, sum, count)) => {
                *sum += r.ln();
                *count += 1;
            }
            None => sums.push((name.to_string(), r.ln(), 1)),
        }
    }
    Ok(sums
        .into_iter()
        .map(|(n, sum, count)| (n, (sum / count as f64).exp()))
        .collect())
}

/// The score: the geometric mean of each metric's ratio, weighted by
/// `weights`. Every weighted metric must have a ratio and every ratio a
/// weight, so a metric can't silently drop out of or sneak into the score.
pub fn weighted_geomean(ratios: &[(&str, f64)], weights: &[(String, f64)]) -> Result<f64, String> {
    let per_metric = metric_ratios(ratios)?;
    if let Some((name, _)) = per_metric
        .iter()
        .find(|(n, _)| !weights.iter().any(|(w, _)| w == n))
    {
        return Err(format!("metric {name} has no weight in weights.tsv"));
    }
    let mut sum = 0.0;
    let mut total = 0.0;
    for (name, w) in weights {
        let r = per_metric
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| *r)
            .ok_or_else(|| format!("metric {name} is weighted but was not measured"))?;
        sum += w * r.ln();
        total += w;
    }
    if total == 0.0 {
        return Err("no weighted metrics".to_string());
    }
    Ok((sum / total).exp())
}
