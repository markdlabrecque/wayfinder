//! Run-to-run noise (issue #453). The same build scores a little differently
//! each run, so `--noise K` measures it K times and stores each metric's
//! spread in `bench/out/noise.json`. A change from the accepted baseline only
//! counts as a win or a loss when it is bigger than that spread. The file
//! records the settings it was measured under and is only reused under the
//! same ones (#481).

use std::path::Path;

use serde_json::{Value, json};

use crate::baseline::{SolrKey, read_json, write_json};

/// The lowest and highest value one metric took over the noise runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    pub min: f64,
    pub max: f64,
}

impl Spread {
    pub fn width(&self) -> f64 {
        self.max - self.min
    }
}

/// How a change from the baseline compares with the noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Win,
    Loss,
    Noise,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Win => "win",
            Verdict::Loss => "loss",
            Verdict::Noise => "noise",
        }
    }
}

/// Each metric's spread over `runs`, in the first run's metric order. Every
/// run must report the same metrics in the same order.
pub fn measure_noise(runs: &[Vec<(String, f64)>]) -> Result<Vec<(String, Spread)>, String> {
    let first = runs.first().ok_or("no runs to measure noise from")?;
    let mut spread: Vec<(String, Spread)> = first
        .iter()
        .map(|(n, v)| (n.clone(), Spread { min: *v, max: *v }))
        .collect();
    for (i, run) in runs.iter().enumerate().skip(1) {
        let names = |r: &[(String, f64)]| r.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        if names(run) != names(first) {
            return Err(format!("run {} measured different metrics", i + 1));
        }
        for ((_, s), (_, v)) in spread.iter_mut().zip(run) {
            s.min = s.min.min(*v);
            s.max = s.max.max(*v);
        }
    }
    Ok(spread)
}

/// Everything a noise measurement depends on (#481). A spread measured under
/// other settings says nothing about this run's noise.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseSettings {
    pub docs: usize,
    pub iters: usize,
    pub cold: usize,
    /// The key of the Solr baseline every noise run was scored against.
    pub solr_key: SolrKey,
}

impl NoiseSettings {
    fn to_json(&self) -> Value {
        json!({
            "docs": self.docs,
            "iters": self.iters,
            "cold": self.cold,
            "solr_key": self.solr_key.to_json(),
        })
    }
}

fn spread_json(spread: &[(String, Spread)]) -> Value {
    spread
        .iter()
        .map(|(n, s)| json!({"metric": n, "min": s.min, "max": s.max}))
        .collect()
}

fn parse_spread(path: &Path, v: &Value) -> Result<Vec<(String, Spread)>, String> {
    let bad = || format!("{}: not a list of metric spreads", path.display());
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(
            |r| match (r["metric"].as_str(), r["min"].as_f64(), r["max"].as_f64()) {
                (Some(n), Some(min), Some(max)) => Ok((n.to_string(), Spread { min, max })),
                _ => Err(bad()),
            },
        )
        .collect()
}

/// Saves `spread`, replacing whatever was saved before.
pub fn save_noise(path: &Path, spread: &[(String, Spread)]) -> Result<(), String> {
    write_json(path, &spread_json(spread))
}

/// The saved spread, `None` if noise has not been measured yet. A file that
/// can't be read is an error, not a silent "no noise".
pub fn load_noise(path: &Path) -> Result<Option<Vec<(String, Spread)>>, String> {
    let Some(v) = read_json(path)? else {
        return Ok(None);
    };
    parse_spread(path, &v).map(Some)
}

/// Saves `spread` with the `settings` it was measured under.
pub fn save_noise_for(
    path: &Path,
    settings: &NoiseSettings,
    spread: &[(String, Spread)],
) -> Result<(), String> {
    write_json(
        path,
        &json!({"settings": settings.to_json(), "spread": spread_json(spread)}),
    )
}

/// The spread saved under `settings`, `None` if noise has not been measured
/// yet or was measured under other settings, see [`load_noise`].
pub fn load_noise_for(
    path: &Path,
    settings: &NoiseSettings,
) -> Result<Option<Vec<(String, Spread)>>, String> {
    let Some(saved) = read_json(path)? else {
        return Ok(None);
    };
    if saved["settings"] != settings.to_json() {
        return Ok(None);
    }
    parse_spread(path, &saved["spread"]).map(Some)
}

/// Lower is better, so a drop from `base` to `new` bigger than the spread's
/// width is a win, a rise bigger than it a loss, and anything else noise.
pub fn classify(base: f64, new: f64, spread: &Spread) -> Verdict {
    let delta = new - base;
    let width = spread.width();
    if delta < -width {
        Verdict::Win
    } else if delta > width {
        Verdict::Loss
    } else {
        Verdict::Noise
    }
}
