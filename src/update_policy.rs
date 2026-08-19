//! Request-level update options shared by JSON updates and extracted documents.

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
        }
        if let Some(ms) = self.commit_within_ms {
            index.schedule_commit(ms);
        }
        Ok(())
    }
}
