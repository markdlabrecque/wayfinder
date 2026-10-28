//! Saved baselines (issue #454), so each tuning step only re-measures
//! Wayfinder. Solr's results are saved once under a key of everything that
//! changes them (dataset size, query file, Solr image, limits) and reused
//! until the key changes. The last accepted Wayfinder score is saved apart so
//! each run can print its change from it.

use std::path::Path;

use serde_json::{Value, json};

/// Everything a saved Solr measurement depends on. Any field differing from
/// the saved one makes the saved numbers stale.
#[derive(Debug, Clone, PartialEq)]
pub struct SolrKey {
    pub corpus_docs: usize,
    pub queries_hash: String,
    pub solr_image_digest: String,
    pub limits: String,
}

impl SolrKey {
    fn to_json(&self) -> Value {
        json!({
            "corpus_docs": self.corpus_docs,
            "queries_hash": self.queries_hash,
            "solr_image_digest": self.solr_image_digest,
            "limits": self.limits,
        })
    }
}

/// FNV-1a 64 of `bytes` as hex: stable across Rust releases, unlike std's
/// `DefaultHasher`, so a saved key stays valid after a toolchain upgrade.
pub fn hash_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Saves Solr's results under `key`, replacing whatever was saved before.
pub fn save_solr_json(path: &Path, key: &SolrKey, solr: &Value) -> Result<(), String> {
    write_json(path, &json!({"key": key.to_json(), "solr": solr}))
}

/// The saved Solr results if they were saved under `key`, `None` if nothing
/// is saved or the key changed. A file that can't be read is an error, not a
/// silent re-measure.
pub fn load_solr_json(path: &Path, key: &SolrKey) -> Result<Option<Value>, String> {
    let Some(saved) = read_json(path)? else {
        return Ok(None);
    };
    if saved["key"] != key.to_json() {
        return Ok(None);
    }
    match &saved["solr"] {
        Value::Null => Err(format!("{}: no solr results", path.display())),
        v => Ok(Some(v.clone())),
    }
}

/// Saves named Solr metrics under `key`, in order.
pub fn save_solr(path: &Path, key: &SolrKey, metrics: &[(String, f64)]) -> Result<(), String> {
    let pairs: Vec<Value> = metrics.iter().map(|(n, v)| json!([n, v])).collect();
    save_solr_json(path, key, &Value::Array(pairs))
}

/// The named Solr metrics saved under `key`, see [`load_solr_json`].
pub fn load_solr(path: &Path, key: &SolrKey) -> Result<Option<Vec<(String, f64)>>, String> {
    let Some(v) = load_solr_json(path, key)? else {
        return Ok(None);
    };
    let bad = || {
        format!(
            "{}: solr results are not [name, number] pairs",
            path.display()
        )
    };
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|p| match (p[0].as_str(), p[1].as_f64()) {
            (Some(n), Some(x)) => Ok((n.to_string(), x)),
            _ => Err(bad()),
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// Saves `score` as the accepted Wayfinder baseline.
pub fn save_wf(path: &Path, score: f64) -> Result<(), String> {
    write_json(path, &json!({"score": score}))
}

/// The accepted Wayfinder score, `None` if nothing has been accepted yet.
pub fn load_wf(path: &Path) -> Result<Option<f64>, String> {
    let Some(saved) = read_json(path)? else {
        return Ok(None);
    };
    saved["score"]
        .as_f64()
        .map(Some)
        .ok_or_else(|| format!("{}: no score", path.display()))
}

/// One line: this run's change from the accepted score. Lower scores are
/// better, so an improvement shows as a negative percent.
pub fn delta_line(accepted: Option<f64>, score: f64) -> String {
    match accepted {
        Some(base) => format!(
            "vs accepted {base:.3}: {:+.1}%",
            (score - base) / base * 100.0
        ),
        None => "no accepted baseline".to_string(),
    }
}
