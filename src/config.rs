//! Server-level config TOML (PRD §6) — distinct from the per-core schema.
//!
//! Every knob is optional with a sane default, and a missing file means all
//! defaults, so `app()` stays a zero-config entry point. Unknown keys are a
//! hard error: this file is operator-facing, and a silently ignored typo is
//! how a tuning knob "stops working" without anyone noticing. That is the one
//! place strictness is a feature — request params go the other way (Solr
//! ignores unknown params, findings fact 8), which is what `strict_params`
//! flips for development.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
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
    pub admin: Admin,
    #[serde(skip)]
    pub auth: Option<AuthConfig>,
}

/// Optional HTTP Basic credentials. Only a SHA-256 digest is retained after
/// parsing so neither the username nor password can be emitted by accidental
/// config debug output.
#[derive(Clone)]
pub struct AuthConfig {
    credential_digest: [u8; 32],
}

impl AuthConfig {
    pub(crate) fn matches(&self, presented: &[u8]) -> bool {
        let presented_digest = Sha256::digest(presented);
        self.credential_digest
            .as_slice()
            .ct_eq(presented_digest.as_slice())
            .into()
    }
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig").finish_non_exhaustive()
    }
}

impl AuthConfig {
    fn from_credentials(username: &str, password: &str) -> Result<Self> {
        if username.is_empty() || password.is_empty() {
            bail!("auth.username and auth.password must both be non-empty");
        }
        if username.contains(':') {
            bail!("auth.username must not contain `:`");
        }
        if has_ascii_control(username) || has_ascii_control(password) {
            bail!("auth.username and auth.password must not contain ASCII control characters");
        }

        let mut hasher = Sha256::new();
        hasher.update(username.as_bytes());
        hasher.update(b":");
        hasher.update(password.as_bytes());
        Ok(Self {
            credential_digest: hasher.finalize().into(),
        })
    }
}

/// Parses the separately extracted `[auth]` table without passing credential
/// values through serde's diagnostics, which may render their values.
fn parse_auth(value: toml::Value) -> Result<AuthConfig> {
    let table = value
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("auth must be a table"))?;

    for key in table.keys() {
        if key != "username" && key != "password" {
            bail!("unknown auth key `{key}`");
        }
    }

    let username = table
        .get("username")
        .ok_or_else(|| anyhow::anyhow!("auth.username is required"))?
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("auth.username must be a string"))?;
    let password = table
        .get("password")
        .ok_or_else(|| anyhow::anyhow!("auth.password is required"))?
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("auth.password must be a string"))?;

    AuthConfig::from_credentials(username, password)
}

/// RFC 7617 forbids control characters in both components of Basic credentials.
fn has_ascii_control(value: &str) -> bool {
    value.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
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
    /// Hard cap, in bytes, on an incoming request body (issue #64). Wired to
    /// an `axum::extract::DefaultBodyLimit` layer in `src/lib.rs::build()`,
    /// which otherwise defaults to axum's bare 2MB cap for every extractor
    /// that buffers the body (`Bytes`, `Json`, ...) — too small for a
    /// realistic bulk `/update`.
    ///
    /// Solr's own `requestParsers` caps (`formdataUploadLimitInKB`,
    /// `multipartUploadLimitInKB`) govern form-urlencoded and multipart
    /// uploads specifically, not the raw `application/json` body a bulk
    /// `/update` actually sends, and the captured configset leaves them
    /// unset (finding 79, `docs/solr-ref-findings.md`) — so this default
    /// could not be verified hermetically against a real Solr max-request-size
    /// setting. 10MB is a deliberate, round headroom figure over the largest
    /// known captured fixture (~7KB in `solr-ref/responses/`), operator
    /// overridable like every other resource knob here.
    pub max_body_size: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Commit {
    /// Hard-commit thresholds, consumed by the update pipeline (issue #9):
    /// `autocommit_max_docs` fires a commit on the Nth uncommitted doc,
    /// `autocommit_max_time` arms a deadline (ms) on the first uncommitted
    /// doc since the last commit. Both make docs VISIBLE (commit + reader
    /// reload), which differs from Solr's own hard-autocommit default of
    /// `openSearcher=false` — a deliberate, documented Wayfinder divergence
    /// (operator-config behaviour, not wire format), not a bug.
    pub autocommit_max_docs: Option<u64>,
    pub autocommit_max_time: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Admin {
    /// The `lucene.solr-spec-version` value reported by `/admin/info/system`
    /// and `<core>/admin/system` (issue #59). `search_api_solr`'s
    /// `SolrConnector::getSolrVersion()` (finding 78) reads this field alone
    /// to detect the Solr version, then regex-captures the leading
    /// `major.minor.patch` and feeds it to the module's own
    /// `version_compare()` gates.
    ///
    /// Default `"9.0.0"` per PRD open question 2 (§10): report the LOWEST
    /// version whose feature set the module's `version_compare()` gates
    /// would unlock that Wayfinder actually implements, never a higher one
    /// that invites an unsupported feature (e.g. `payload_score`, gated at
    /// major >= 6, is unimplemented regardless of version, but there is no
    /// reason to report higher than the 9.x branch the capture's generated
    /// `schema.xml` already targets — see issue #59's spec for the full
    /// reasoning). This value is intentionally unclamped: an operator who
    /// overrides it is trusted to know the compatibility risk.
    pub reported_solr_version: String,
}

impl Default for Admin {
    fn default() -> Self {
        Admin {
            reported_solr_version: "9.0.0".to_string(),
        }
    }
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
            max_body_size: 10_000_000,
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
        // Parse without deserializing first: TOML's source-aware syntax errors
        // include excerpts of `raw`, which could disclose configured credentials.
        // Deserializing the value afterward retains useful unknown-field names.
        let mut value: toml::Value =
            toml::from_str(raw).map_err(|_| anyhow::anyhow!("invalid server config TOML"))?;
        let auth = value
            .as_table_mut()
            .expect("a TOML document is always a table")
            .remove("auth")
            .map(parse_auth)
            .transpose()?;
        let mut config: ServerConfig = value.try_into().context("parsing server config TOML")?;
        config.auth = auth;
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
        assert_eq!(config.resources.max_body_size, 10_000_000);
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

    #[test]
    fn auth_rejects_invalid_rfc_7617_credentials() {
        for config in [
            "[auth]\nusername = \"\"\npassword = \"secret\"\n",
            "[auth]\nusername = \"operator\"\npassword = \"\"\n",
            "[auth]\nusername = \"operator:name\"\npassword = \"secret\"\n",
            "[auth]\nusername = \"operator\\u0000\"\npassword = \"secret\"\n",
            "[auth]\nusername = \"operator\"\npassword = \"secret\\u007f\"\n",
        ] {
            ServerConfig::parse(config).expect_err("invalid Basic credentials must be rejected");
        }
    }

    #[test]
    fn auth_allows_a_colon_in_the_password() {
        let config = ServerConfig::parse(
            "[auth]\nusername = \"operator\"\npassword = \"secret:with:colons\"\n",
        )
        .expect("an RFC 7617 password may contain colons");
        assert!(
            config
                .auth
                .as_ref()
                .expect("[auth] must be present")
                .matches(b"operator:secret:with:colons")
        );
    }

    #[test]
    fn auth_rejects_non_string_credentials_without_echoing_them() {
        let numeric_sentinel = "8675309123456789";
        let err = ServerConfig::parse(&format!(
            "[auth]\nusername = \"operator\"\npassword = {numeric_sentinel}\n"
        ))
        .expect_err("numeric passwords must be rejected");
        let message = format!("{err:#}");
        assert!(message.contains("auth.password must be a string"));
        assert!(
            !message.contains(numeric_sentinel),
            "semantic errors must not echo credential values: {message}"
        );

        let err = ServerConfig::parse(
            "[auth]\nusername = \"operator\"\npassword = 1979-05-27T07:32:00Z\n",
        )
        .expect_err("datetime passwords must be rejected");
        assert!(format!("{err:#}").contains("auth.password must be a string"));
    }

    #[test]
    fn auth_must_be_a_table() {
        let err =
            ServerConfig::parse("auth = \"operator:secret\"\n").expect_err("auth must be a table");
        assert!(format!("{err:#}").contains("auth must be a table"));
    }

    #[test]
    fn auth_requires_exactly_username_and_password() {
        let err = ServerConfig::parse("[auth]\nusername = \"operator\"\n")
            .expect_err("auth.password is required");
        assert!(format!("{err:#}").contains("auth.password is required"));

        let sentinel = "AUTH_UNKNOWN_SECRET_MUST_NOT_LEAK";
        let err = ServerConfig::parse(&format!(
            "[auth]\nusername = \"operator\"\npassword = \"secret\"\ntoken = \"{sentinel}\"\n"
        ))
        .expect_err("unknown auth keys must be rejected");
        let message = format!("{err:#}");
        assert!(message.contains("unknown auth key `token`"));
        assert!(
            !message.contains(sentinel),
            "unknown-key errors must not echo credential values: {message}"
        );
    }

    #[test]
    fn syntax_errors_do_not_echo_credentials() {
        let sentinel = "AUTH_PARSE_SECRET_MUST_NOT_LEAK";
        let err = ServerConfig::parse(&format!(
            "[auth]\nusername = \"operator\"\npassword = \"{sentinel}\"\nnot valid TOML"
        ))
        .expect_err("invalid TOML must fail");
        let message = format!("{err:#}");
        assert!(message.contains("invalid server config TOML"));
        assert!(
            !message.contains(sentinel),
            "syntax errors must not echo credential values: {message}"
        );
    }
}
