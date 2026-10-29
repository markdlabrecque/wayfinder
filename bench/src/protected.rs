//! Protected files (issue #457). The tuning loop is judged by Solr's saved
//! answers, the query list, the allowed-diffs list, the weights and the
//! scoring code. A loop that could edit them could "win" by cheating, so the
//! accepted baseline records a hash of each, and a run whose files differ
//! refuses to score until a person re-accepts them.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;

use crate::baseline::{hash_hex, read_json, write_json};

/// Paths under the repo root the loop may not change. A trailing `/` means
/// every file below that directory.
pub const PROTECTED: &[&str] = &[
    "solr-ref/responses/",
    "bench/allowed-diffs.tsv",
    "bench/queries.tsv",
    "bench/weights.tsv",
    "bench/src/score.rs",
    "bench/src/check.rs",
    "bench/src/protected.rs",
    "bench/src/bin/wf-bench.rs",
];

/// Relative path to content hash for every protected file under `root`. A
/// missing file is left out, so deleting one shows up as a change.
// ponytail: FNV-1a 64, the hash the rest of the bench uses. It catches any
// edit made in good faith or by accident, not a crafted collision.
pub fn snapshot(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut hashes = BTreeMap::new();
    for p in PROTECTED {
        if let Some(dir) = p.strip_suffix('/') {
            walk(root, dir, &mut hashes)?;
        } else {
            hash_file(root, p, &mut hashes)?;
        }
    }
    Ok(hashes)
}

fn hash_file(root: &Path, rel: &str, out: &mut BTreeMap<String, String>) -> Result<(), String> {
    let path = root.join(rel);
    match std::fs::read(&path) {
        Ok(bytes) => {
            out.insert(rel.to_string(), hash_hex(&bytes));
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn walk(root: &Path, rel: &str, out: &mut BTreeMap<String, String>) -> Result<(), String> {
    let path = root.join(rel);
    let entries = match std::fs::read_dir(&path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{rel}/{name}");
        let is_dir = entry
            .file_type()
            .map_err(|e| format!("{child}: {e}"))?
            .is_dir();
        if is_dir {
            walk(root, &child, out)?;
        } else {
            hash_file(root, &child, out)?;
        }
    }
    Ok(())
}

/// Saves `hashes` as the accepted protected-file hashes.
pub fn save_hashes(path: &Path, hashes: &BTreeMap<String, String>) -> Result<(), String> {
    write_json(path, &json!({ "hashes": hashes }))
}

/// The accepted protected-file hashes, `None` if nothing has been accepted yet.
pub fn load_hashes(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(saved) = read_json(path)? else {
        return Ok(None);
    };
    serde_json::from_value(saved["hashes"].clone())
        .map(Some)
        .map_err(|e| format!("{}: hashes: {e}", path.display()))
}

/// One line naming the protected files that changed.
pub fn report(changed: &[String]) -> String {
    format!(
        "protected files changed since the accepted baseline: {}",
        changed.join(", ")
    )
}

/// `Ok` when every protected file under `root` matches the hashes saved at
/// `saved`, or nothing has been accepted yet. Otherwise the error names each
/// file edited, added or deleted, and says how a person re-accepts them.
pub fn check_run(root: &Path, saved: &Path) -> Result<(), String> {
    let Some(accepted) = load_hashes(saved)? else {
        return Ok(());
    };
    let now = snapshot(root)?;
    let mut changed: Vec<String> = accepted
        .keys()
        .chain(now.keys())
        .filter(|f| accepted.get(*f) != now.get(*f))
        .cloned()
        .collect();
    changed.sort();
    changed.dedup();
    if changed.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}. Refusing to score: a person must review the change, then run wf-bench --accept-protected",
        report(&changed)
    ))
}
