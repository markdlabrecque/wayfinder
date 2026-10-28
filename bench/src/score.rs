//! The one score the tuning loop chases (issue #452). Every metric becomes a
//! Wayfinder/Solr ratio, inverted for higher-is-better metrics so below 1.0
//! always means Wayfinder wins. The score is the weighted geometric mean of
//! those ratios, with the weights read from `bench/weights.tsv`.

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
