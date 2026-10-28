//! Answer fingerprints (issue #445). Each run hashes Wayfinder's answers per
//! query shape, with timing stripped, so a tuning step that changes any answer
//! shows up even when it still passes the Solr comparison. The digests of the
//! last accepted run are saved apart, next to its score.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::baseline::{hash_hex, read_json, write_json};

/// Keys that vary from run to run without the answer changing.
const TIMING_KEYS: &[&str] = &["QTime"];

fn strip_timing(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for k in TIMING_KEYS {
                m.remove(*k);
            }
            m.values_mut().for_each(strip_timing);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_timing),
        _ => {}
    }
}

/// One digest over a shape's answers, in request order, timing stripped.
pub fn digest(answers: &[Value]) -> String {
    let mut text = String::new();
    for a in answers {
        let mut a = a.clone();
        strip_timing(&mut a);
        // Compact JSON never holds a raw newline, so it separates answers.
        text.push_str(&a.to_string());
        text.push('\n');
    }
    hash_hex(text.as_bytes())
}

/// Saves `digests` as the accepted run's fingerprints.
pub fn save_accepted(path: &Path, digests: &BTreeMap<String, String>) -> Result<(), String> {
    write_json(path, &json!({ "digests": digests }))
}

/// The accepted run's fingerprints, `None` if nothing has been accepted yet.
pub fn load_accepted(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(saved) = read_json(path)? else {
        return Ok(None);
    };
    serde_json::from_value(saved["digests"].clone())
        .map(Some)
        .map_err(|e| format!("{}: digests: {e}", path.display()))
}

/// Shapes whose digest differs, or that only one side has, in name order.
pub fn changed_shapes(
    accepted: &BTreeMap<String, String>,
    now: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut names: Vec<&String> = accepted.keys().chain(now.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|n| accepted.get(*n) != now.get(*n))
        .cloned()
        .collect()
}

/// One line naming the changed shapes, `None` when nothing changed.
pub fn answers_changed_line(shapes: &[String]) -> Option<String> {
    (!shapes.is_empty()).then(|| format!("answers changed: {}", shapes.join(", ")))
}
