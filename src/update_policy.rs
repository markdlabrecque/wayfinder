//! Request-level update options shared by JSON updates and extracted documents.

use serde_json::{Value, json};

use crate::core_index::CoreIndex;
use crate::error::{Envelope, WfError};
use crate::params::Params;

#[derive(Debug, Clone, Copy)]
pub(crate) struct UpdatePolicy {
    pub overwrite: bool,
    pub commit_requested: bool,
    commit_within_ms: Option<u64>,
}

impl UpdatePolicy {
    pub(crate) fn from_params(params: &Params) -> Result<Self, WfError> {
        let bool_param = |key: &str, default: bool| {
            params
                .bool_or(key, default)
                .map_err(|error| error.envelope(Envelope::NoParams))
        };
        // Parse both booleans before combining them. Short-circuiting here
        // would hide an invalid `softCommit` behind `commit=true`.
        let commit = bool_param("commit", false)?;
        let soft_commit = bool_param("softCommit", false)?;
        let overwrite = bool_param("overwrite", true)?;
        let commit_within_ms = params
            .get("commitWithin")
            .and_then(|value| value.parse::<u64>().ok());
        Ok(Self {
            overwrite,
            commit_requested: commit || soft_commit,
            commit_within_ms,
        })
    }

    /// Applies the post-write commit behavior common to both update routes.
    pub(crate) fn finish(self, index: &CoreIndex) -> anyhow::Result<()> {
        if self.commit_requested {
            index.commit()?;
        } else if let Some(ms) = self.commit_within_ms {
            index.schedule_commit(ms);
        }
        Ok(())
    }
}

/// The bare `{"responseHeader":{"status":0,"QTime":0}}` envelope every
/// `/update` success answers with, for every command shape (finding 46) —
/// never a `params` echo, never per-command keys. Shared by JSON updates and
/// by an extracted document that was indexed rather than rendered, so the
/// envelope has one owner.
///
/// Under `omitHeader=true` that leaves `{}`: the bare envelope has no other
/// key to survive the header's removal.
///
/// ponytail: unfixtured for `/update` specifically. `search_api_solr` only
/// ever sends `omitHeader=false` here (`solr-ref/search-api/trace/00001.json`),
/// so no capture shows `/update` under `omitHeader=true`. This generalizes
/// from `/select`/`/mlt`/`/terms`, which all gate on the same param and are
/// fixture-pinned; the alternative reading ("`/update` never suppresses") is
/// possible but has nothing behind it. A capture of a real `solr:9`
/// `/update?commit=true&omitHeader=true` settles it.
pub(crate) fn success_body(params: &Params) -> Value {
    if params.omit_header() {
        return json!({});
    }
    json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        }
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::config::ServerConfig;

    use super::*;

    const SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

    fn test_server() -> (crate::AppServer, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let schema = dir.path().join("schema.toml");
        std::fs::write(&schema, SCHEMA).expect("write schema");
        let data = dir.path().join("data");
        std::fs::create_dir(&data).expect("create data dir");
        let server = crate::build(&schema, &data, ServerConfig::default()).expect("build app");
        (server, dir)
    }

    #[test]
    fn explicit_commit_absorbs_commit_within() {
        for immediate in ["commit=true", "softCommit=true"] {
            let (server, _dir) = test_server();
            let index = &server.shutdown.0.index;
            let params = Params::parse(&format!("{immediate}&commitWithin=60000"));
            let policy = UpdatePolicy::from_params(&params).expect("parse update policy");

            policy.finish(index).expect("finish update policy");

            assert!(
                !index.has_scheduled_commit(),
                "{immediate} must absorb commitWithin instead of arming another deadline"
            );
        }
    }
}
