//! Run-to-run noise (issue #453). The same build scores a little differently
//! each run, so `--noise K` measures it K times and stores each metric's
//! spread in `bench/out/noise.json`. A change from the accepted baseline only
//! counts as a win or a loss when it is bigger than that spread.

use std::path::Path;

use serde_json::{Value, json};

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

/// Saves `spread`, replacing whatever was saved before.
pub fn save_noise(path: &Path, spread: &[(String, Spread)]) -> Result<(), String> {
    let rows: Vec<Value> = spread
        .iter()
        .map(|(n, s)| json!({"metric": n, "min": s.min, "max": s.max}))
        .collect();
    std::fs::write(path, serde_json::to_string_pretty(&rows).unwrap())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The saved spread, `None` if noise has not been measured yet. A file that
/// can't be read is an error, not a silent "no noise".
pub fn load_noise(path: &Path) -> Result<Option<Vec<(String, Spread)>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let bad = || format!("{}: not a list of metric spreads", path.display());
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|r| match (r["metric"].as_str(), r["min"].as_f64(), r["max"].as_f64()) {
            (Some(n), Some(min), Some(max)) => Ok((n.to_string(), Spread { min, max })),
            _ => Err(bad()),
        })
        .collect::<Result<_, _>>()
        .map(Some)
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
