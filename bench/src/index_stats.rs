//! Indexing throughput for `wf-bench` (issue #449). Both engines get the same
//! docs in the same batches; the time runs from the first batch to the final
//! commit, so the rates are directly comparable.

use serde_json::{Value, json};

/// Turns raw counts into the `index_*` fields of an engine's report.
pub fn index_stats(docs: u64, bytes: u64, secs: f64) -> Value {
    json!({
        "index_docs": docs,
        "index_bytes": bytes,
        "index_secs": secs,
        "index_docs_per_s": docs as f64 / secs,
        "index_bytes_per_s": bytes as f64 / secs,
    })
}
