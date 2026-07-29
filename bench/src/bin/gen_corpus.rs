//! Writes a deterministically generated corpus as Solr-update-shaped JSON
//! batches (an array of doc objects per file) into an output directory,
//! plus the matching Wayfinder/Solr schema TOML -- both consumed by
//! `bench/run.sh`'s indexing step for the 50k/2M benchmark runs (issue
//! #13).
//!
//! Batched rather than one giant array: both a real bulk-load tool and
//! Wayfinder's own default `axum::body::Bytes` extractor limit (2 MB,
//! axum's `DefaultBodyLimit`, not overridden -- a real product constraint
//! this benchmark surfaced, out of scope to change from here) mean a 50k-doc
//! corpus has to go in over more than one request.
//!
//! Usage: `gen_corpus <seed> <size> <out-dir> [batch-size]`
//! (`batch-size` defaults to 2000 docs/file, comfortably under 2 MB.)
//!
//! Hand-rolled JSON/TOML string emission rather than a `serde_json`/`toml`
//! dependency, per this crate's no-dependency constraint (`Cargo.toml`).

use wayfinder_bench::corpus::{Doc, generate};

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

fn doc_json(doc: &Doc) -> String {
    format!(
        "{{\"id\":\"{}\",\"title\":\"{}\",\"body\":\"{}\",\"category\":[{}]}}",
        json_escape(&doc.id),
        json_escape(&doc.title),
        json_escape(&doc.body),
        doc.category
            .iter()
            .map(|c| format!("\"{}\"", json_escape(c)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: gen_corpus <seed> <size> <out-dir> [batch-size]";
    let seed: u64 = args
        .next()
        .expect(usage)
        .parse()
        .expect("seed must be a u64");
    let size: usize = args
        .next()
        .expect(usage)
        .parse()
        .expect("size must be a usize");
    let out_dir = args.next().expect(usage);
    let batch_size: usize = args
        .next()
        .map(|s| s.parse().expect("batch-size must be a usize"))
        .unwrap_or(2000);

    std::fs::create_dir_all(&out_dir).expect("create out-dir");

    let docs = generate(seed, size);

    let mut batch_count = 0usize;
    for (batch_idx, chunk) in docs.chunks(batch_size.max(1)).enumerate() {
        let mut json = String::with_capacity(chunk.len() * 128);
        json.push('[');
        for (i, doc) in chunk.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&doc_json(doc));
        }
        json.push(']');
        let path = format!("{out_dir}/batch-{batch_idx:05}.json");
        std::fs::write(&path, json).expect("write corpus batch JSON");
        batch_count += 1;
    }

    let schema_path = format!("{out_dir}/schema.toml");
    let schema = r#"[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true
"#;
    std::fs::write(&schema_path, schema).expect("write schema TOML");

    eprintln!(
        "wrote {} docs across {batch_count} batch file(s) to {out_dir}, schema to {schema_path}",
        docs.len()
    );
}
