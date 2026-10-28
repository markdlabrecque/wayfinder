//! Real-text corpus for `wf-bench` (issue #440): English Wikipedia articles
//! read from the pinned parquet shards `bench/fetch-wiki.sh` downloads.
//!
//! Rows are read in shard order, so the first N docs are always the same N.
//! The dataset only has `id`, `url`, `title`, and `text`; the extra fields are
//! derived from those so facet, range, sort, and stats queries have something
//! to work on.

use std::fs::File;
use std::path::Path;

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;

#[derive(Debug, Clone, PartialEq)]
pub struct WikiDoc {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Body length in bytes.
    pub len: i64,
    /// `short` / `medium` / `long` / `xl`, from `len`.
    pub len_bucket: &'static str,
    /// Upper-cased first ASCII letter of the title, or `other`.
    pub initial: String,
}

pub fn len_bucket(len: i64) -> &'static str {
    match len {
        ..2_000 => "short",
        2_000..10_000 => "medium",
        10_000..40_000 => "long",
        _ => "xl",
    }
}

pub fn initial(title: &str) -> String {
    match title.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        _ => "other".to_string(),
    }
}

/// Reads the first `n` articles from `data_dir/wiki-*.parquet`, in shard order.
/// Panics when the shards hold fewer than `n` rows, so a short download can
/// never silently shrink the corpus.
pub fn load(data_dir: &Path, n: usize) -> Vec<WikiDoc> {
    let mut shards: Vec<_> = std::fs::read_dir(data_dir)
        .unwrap_or_else(|e| panic!("{}: {e} (run bench/fetch-wiki.sh)", data_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with("wiki-") && f.ends_with(".parquet"))
        })
        .collect();
    shards.sort();

    let mut docs = Vec::with_capacity(n);
    for shard in shards {
        let reader = SerializedFileReader::new(File::open(&shard).expect("open shard"))
            .expect("read parquet");
        for row in reader.get_row_iter(None).expect("row iter") {
            if docs.len() == n {
                return docs;
            }
            let row = row.expect("parquet row");
            let (mut id, mut title, mut body) = (None, None, None);
            for (name, field) in row.get_column_iter() {
                if let Field::Str(s) = field {
                    match name.as_str() {
                        "id" => id = Some(s.clone()),
                        "title" => title = Some(s.clone()),
                        "text" => body = Some(s.clone()),
                        _ => {}
                    }
                }
            }
            let (id, title, body) = (id.expect("id"), title.expect("title"), body.expect("text"));
            let len = body.len() as i64;
            docs.push(WikiDoc {
                initial: initial(&title),
                len_bucket: len_bucket(len),
                len,
                id,
                title,
                body,
            });
        }
    }
    assert_eq!(docs.len(), n, "only {} articles on disk", docs.len());
    docs
}
