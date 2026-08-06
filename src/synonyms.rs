//! Per-core, query-side synonym tables (issue #389).
//!
//! The resource is deliberately separate from the Tantivy index: changing an
//! equivalence group affects future query analysis immediately, but never writes
//! alternate spellings into postings or requires a reindex.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, bail};

pub const FILE_NAME: &str = "synonyms.txt";

#[derive(Debug, Clone, Default)]
struct SynonymTable {
    groups: Vec<Vec<String>>,
    expansions: HashMap<String, Vec<String>>,
}

/// Canonical form at the synonym filter insertion point. Schema owns this
/// analyzer contract so persisted members cannot drift from query analysis.
fn canonical_member(member: &str) -> Result<String> {
    crate::schema::canonicalize_synonym_member(member)
}

impl SynonymTable {
    fn from_groups(groups: Vec<Vec<String>>) -> Result<Self> {
        let mut canonical_groups = HashSet::new();
        let mut members = HashSet::new();
        let mut expansions = HashMap::new();
        for group in &groups {
            if group.len() < 2 {
                bail!("each synonym group must contain at least two distinct single tokens");
            }
            let mut seen = HashSet::new();
            for term in group {
                if !seen.insert(term) {
                    bail!("a synonym group cannot contain a duplicate member `{term}`");
                }
                if !members.insert(term) {
                    bail!("a synonym member cannot belong to multiple groups: `{term}`");
                }
            }
            let canonical: BTreeSet<&str> = group.iter().map(String::as_str).collect();
            if !canonical_groups.insert(canonical) {
                bail!("duplicate synonym group");
            }
            for term in group {
                expansions.insert(
                    term.clone(),
                    group
                        .iter()
                        .filter(|other| *other != term)
                        .cloned()
                        .collect(),
                );
            }
        }
        Ok(Self { groups, expansions })
    }

    fn parse(text: &str) -> Result<Self> {
        let mut groups = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let group = line
                .split(',')
                .map(|member| canonical_member(member.trim()))
                .collect::<Result<Vec<_>>>()?;
            groups.push(group);
        }
        Self::from_groups(groups)
    }

    fn render(&self) -> String {
        self.groups
            .iter()
            .map(|group| group.join(","))
            .collect::<Vec<_>>()
            .join("\n")
            + if self.groups.is_empty() { "" } else { "\n" }
    }
}

/// Shared by every query analyzer of one core. Poisoned locks are recovered:
/// validation and replacement never panic while holding this lock, but a
/// previous unrelated panic must not turn all queries into failures.
#[derive(Clone)]
pub struct SynonymResource {
    path: PathBuf,
    table: Arc<RwLock<SynonymTable>>,
    /// Serializes the durable rename and the in-memory swap as one operation.
    /// Without it, two successful replacements can leave the file from one
    /// request paired with the live table from the other.
    replace_lock: Arc<Mutex<()>>,
}

impl SynonymResource {
    /// Loads `<core data dir>/synonyms.txt` before the first query. A missing
    /// file means an empty table; malformed durable state fails startup rather
    /// than being silently ignored.
    pub fn open(core_dir: &Path) -> Result<Self> {
        fs::create_dir_all(core_dir)
            .with_context(|| format!("creating synonym directory {}", core_dir.display()))?;
        let path = core_dir.join(FILE_NAME);
        let table = match fs::read_to_string(&path) {
            Ok(text) => SynonymTable::parse(&text)
                .with_context(|| format!("parsing synonym table {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SynonymTable::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading synonym table {}", path.display()));
            }
        };
        Ok(Self {
            path,
            table: Arc::new(RwLock::new(table)),
            replace_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn expansions(&self, term: &str) -> Vec<String> {
        let table = self
            .table
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table.expansions.get(term).cloned().unwrap_or_default()
    }

    pub fn groups(&self) -> Vec<Vec<String>> {
        self.table
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .groups
            .clone()
    }

    /// Validates all input before touching either state. The durable replacement
    /// is a same-directory temp-file rename; only after it succeeds does the
    /// live Arc<RwLock> table swap, so a rejected or failed save preserves both.
    pub fn replace(&self, submitted: &str) -> Result<()> {
        // Validate before acquiring the operation lock: rejected input is pure
        // and never delays a valid save. Successful replace calls then hold it
        // across both persistence and publication.
        let candidate = SynonymTable::parse(submitted)?;
        let _operation = self
            .replace_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let rendered = candidate.render();
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("synonym path has no data parent"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("creating synonym temp file in {}", parent.display()))?;
        temporary
            .write_all(rendered.as_bytes())
            .context("writing synonym temp file")?;
        temporary
            .as_file()
            .sync_all()
            .context("syncing synonym temp file")?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!("atomically replacing synonym table {}", self.path.display())
            })?;
        *self
            .table
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = candidate;
        Ok(())
    }
}
