//! Server-level config TOML (PRD §6) — distinct from the per-core schema.
//!
//! Every knob is optional with a sane default, and a missing file means all
//! defaults, so `app()` stays a zero-config entry point. Unknown keys are a
//! hard error: this file is operator-facing, and a silently ignored typo is
//! how a tuning knob "stops working" without anyone noticing. That is the one
//! place strictness is a feature — request params go the other way (Solr
//! ignores unknown params, findings fact 8), which is what `strict_params`
//! flips for development.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tantivy::IndexSettings;
use tantivy::merge_policy::{LogMergePolicy, MergePolicy, NoMergePolicy};
use tantivy::store::Compressor;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Reject unknown request params with a 400 instead of ignoring them.
    /// Default `false`, matching Solr (findings fact 8 and the decision it
    /// forces); `true` is a development aid for finding unimplemented params.
    pub strict_params: bool,
    pub indexing: Indexing,
    pub query: Query,
    pub resources: Resources,
    pub commit: Commit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Indexing {
    /// `IndexWriter` arena size in bytes, across all writer threads.
    pub writer_heap: usize,
    /// Indexing thread count. Defaults to 1: a single writer thread allocates
    /// doc ids in insertion order, which is what the `AllScoredHits` tie-break
    /// relies on to match Solr's observed ordering of equally scored matches.
    pub writer_threads: usize,
    /// `"log"` (Tantivy's `LogMergePolicy`) or `"no_merge"` for bulk load.
    pub merge_policy: String,
    /// `LogMergePolicy` parameters; ignored under `no_merge`.
    pub merge_min_layer_size: Option<u32>,
    pub merge_level_log_size: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Query {
    /// Per-query time budget in ms. Parsed and exposed; not yet enforced.
    // ponytail: accepted-but-inert. Tantivy has no query deadline, so
    // enforcing this needs a deadline check inside the collector — worth doing
    // when a query can actually run long (post-#3 aggregation, large corpora).
    pub time_allowed: Option<u64>,
    /// Hard cap on `rows`, so a bad client cannot ask for a million documents.
    /// Solr has no equivalent request cap, so over-limit requests are clamped
    /// rather than rejected — a clamp keeps working clients working.
    pub rows_limit: usize,
    /// Hard cap on `facet.limit`, clamping over-limit requests like
    /// `rows_limit` does. Live since issue #3 landed `facet.limit`.
    pub facet_limit_max: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Resources {
    /// `"none"` or `"lz4"`. Applied when the index is first created; opening an
    /// existing index keeps the settings it was built with.
    pub doc_store_compression: String,
    pub doc_store_blocksize: usize,
    /// Accepted and exposed, but inert: Tantivy 0.26 creates searchers on
    /// demand from the reader rather than from a fixed pool, so there is
    /// nothing to size. Kept because PRD §6 names it.
    // ponytail: accepted-but-inert, no Tantivy equivalent to wire it to. If a
    // searcher pool ever exists, this is the knob; until then it is a
    // documented no-op rather than a lie about what it does.
    pub searcher_pool_size: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Commit {
    /// Hard-commit thresholds. Parsed and exposed here; consumed by the update
    /// pipeline (issue #9).
    pub autocommit_max_docs: Option<u64>,
    pub autocommit_max_time: Option<u64>,
}

impl Default for Indexing {
    fn default() -> Self {
        Indexing {
            writer_heap: 32_000_000,
            writer_threads: 1,
            merge_policy: "log".to_string(),
            merge_min_layer_size: None,
            merge_level_log_size: None,
        }
    }
}

impl Default for Query {
    fn default() -> Self {
        Query {
            time_allowed: None,
            rows_limit: 10_000,
            facet_limit_max: 1_000,
        }
    }
}

impl Default for Resources {
    fn default() -> Self {
        Resources {
            doc_store_compression: "lz4".to_string(),
            doc_store_blocksize: 16_384,
            searcher_pool_size: 1,
        }
    }
}

impl ServerConfig {
    /// Loads the config from `path`. A missing file is all defaults; an
    /// unreadable or invalid one is an error.
    pub fn load(path: &Path) -> Result<ServerConfig> {
        if !path.exists() {
            return Ok(ServerConfig::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading server config {}", path.display()))?;
        ServerConfig::parse(&raw).with_context(|| format!("in server config {}", path.display()))
    }

    pub fn parse(raw: &str) -> Result<ServerConfig> {
        let config: ServerConfig = toml::from_str(raw).context("parsing server config TOML")?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects values that only fail much later (or silently) if left alone.
    fn validate(&self) -> Result<()> {
        self.compressor()?;
        self.merge_policy()?;
        if self.indexing.writer_threads == 0 {
            bail!("indexing.writer_threads must be at least 1");
        }
        Ok(())
    }

    fn compressor(&self) -> Result<Compressor> {
        match self.resources.doc_store_compression.as_str() {
            "none" => Ok(Compressor::None),
            "lz4" => Ok(Compressor::Lz4),
            other => bail!(
                "unsupported resources.doc_store_compression `{other}` (expected `none` or `lz4`)"
            ),
        }
    }

    /// Tantivy index settings for a *newly created* index.
    pub fn index_settings(&self) -> Result<IndexSettings> {
        Ok(IndexSettings {
            docstore_compression: self.compressor()?,
            docstore_blocksize: self.resources.doc_store_blocksize,
            ..IndexSettings::default()
        })
    }

    pub fn merge_policy(&self) -> Result<Box<dyn MergePolicy>> {
        match self.indexing.merge_policy.as_str() {
            "no_merge" => Ok(Box::new(NoMergePolicy)),
            "log" => {
                let mut policy = LogMergePolicy::default();
                if let Some(size) = self.indexing.merge_min_layer_size {
                    policy.set_min_layer_size(size);
                }
                if let Some(size) = self.indexing.merge_level_log_size {
                    policy.set_level_log_size(size);
                }
                Ok(Box::new(policy))
            }
            other => {
                bail!("unsupported indexing.merge_policy `{other}` (expected `log` or `no_merge`)")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let config = ServerConfig::parse("").expect("empty config is valid");
        assert!(!config.strict_params, "Solr ignores unknown params");
        assert_eq!(config.indexing.writer_heap, 32_000_000);
        assert_eq!(config.indexing.writer_threads, 1);
        assert_eq!(config.indexing.merge_policy, "log");
        assert_eq!(config.query.rows_limit, 10_000);
        assert_eq!(config.query.facet_limit_max, 1_000);
        assert_eq!(config.query.time_allowed, None);
        assert_eq!(config.resources.doc_store_compression, "lz4");
        assert_eq!(config.resources.doc_store_blocksize, 16_384);
        assert_eq!(config.commit.autocommit_max_docs, None);
    }

    #[test]
    fn missing_file_is_all_defaults() {
        let config = ServerConfig::load(Path::new("/nonexistent/wayfinder.toml"))
            .expect("a missing config file means defaults");
        assert_eq!(config.query.rows_limit, 10_000);
    }

    #[test]
    fn a_partial_section_keeps_the_other_defaults() {
        let config = ServerConfig::parse("[indexing]\nwriter_threads = 3\n").expect("valid");
        assert_eq!(config.indexing.writer_threads, 3);
        assert_eq!(
            config.indexing.writer_heap, 32_000_000,
            "unset knobs in a present section stay at their defaults"
        );
    }

    #[test]
    fn unknown_key_is_rejected_by_name() {
        let err = ServerConfig::parse("strictparams = true\n").expect_err("typo must fail");
        assert!(format!("{err:#}").contains("strictparams"));
    }

    #[test]
    fn unknown_key_in_a_section_is_rejected_by_name() {
        let err = ServerConfig::parse("[query]\nrows_limitt = 5\n").expect_err("typo must fail");
        assert!(format!("{err:#}").contains("rows_limitt"));
    }

    #[test]
    fn bad_enum_values_are_rejected_by_value() {
        let err = ServerConfig::parse("[indexing]\nmerge_policy = \"sometimes\"\n")
            .expect_err("bad merge policy must fail");
        assert!(format!("{err:#}").contains("sometimes"));

        let err = ServerConfig::parse("[resources]\ndoc_store_compression = \"snappy\"\n")
            .expect_err("bad compressor must fail");
        assert!(format!("{err:#}").contains("snappy"));
    }

    #[test]
    fn zero_writer_threads_is_rejected() {
        let err = ServerConfig::parse("[indexing]\nwriter_threads = 0\n")
            .expect_err("zero threads must fail");
        assert!(format!("{err:#}").contains("writer_threads"));
    }

    #[test]
    fn index_settings_reflect_the_resource_knobs() {
        let config = ServerConfig::parse(
            "[resources]\ndoc_store_compression = \"none\"\ndoc_store_blocksize = 8192\n",
        )
        .expect("valid");
        let settings = config.index_settings().expect("settings");
        assert_eq!(settings.docstore_compression, Compressor::None);
        assert_eq!(settings.docstore_blocksize, 8192);
    }
}
