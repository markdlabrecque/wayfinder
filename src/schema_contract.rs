//! Compatibility and migration for schema state persisted beside an index.
//!
//! Startup crosses this module once, before it opens or creates Tantivy files.
//! A successful return means the stored schema is compatible and the analyzer
//! marker has already been accepted or persisted. The caller may then open the
//! index and persist the current schema through [`Accepted::persist_schema`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tantivy::Index;

use crate::schema::{self, WayfinderSchema};

const SCHEMA_SNAPSHOT_FILE: &str = "wayfinder-schema.toml";
const ANALYZER_MARKER_FILE: &str = "wayfinder-analyzer-contract";

const CURRENT: &str = "text_presets_static_length_v7";
const CURRENT_LEGACY_DYNAMIC: &str = "text_presets_static_length_v7_legacy_dynamic_text";
const V6: &str = "text_presets_uax29_word_delimiter_v6";
const V6_LEGACY_DYNAMIC: &str = "text_presets_uax29_word_delimiter_v6_legacy_dynamic_text";
const V5: &str = "text_presets_uax29_v5";
const V5_LEGACY_DYNAMIC: &str = "text_presets_uax29_v5_legacy_dynamic_text";
const V4: &str = "text_presets_accent_folding_v4";
const V4_LEGACY_DYNAMIC: &str = "text_presets_accent_folding_v4_legacy_dynamic_text";
const V3: &str = "text_en_solr_length_case_v3";
const V3_LEGACY_DYNAMIC: &str = "text_en_porter_compatible_v3_legacy_dynamic_text";
const V2: &str = "text_en_porter_compatible_v2";
const V2_LEGACY_DYNAMIC: &str = "text_en_porter_compatible_v2_legacy_dynamic_text";
const V1: &str = "text_en_stopwords_v1";
const V1_LEGACY_DYNAMIC: &str = "text_en_stopwords_v1_legacy_dynamic_text";

/// Schema state accepted for one startup. Any marker write completes before
/// this value exists; the schema snapshot is delayed until Tantivy opens cleanly.
pub(crate) struct Accepted {
    snapshot_path: PathBuf,
    current_schema: String,
}

impl Accepted {
    pub(crate) fn persist_schema(self) -> Result<()> {
        std::fs::write(&self.snapshot_path, self.current_schema)
            .with_context(|| format!("writing stored schema {}", self.snapshot_path.display()))
    }
}

/// Returns every persisted schema-contract artifact that must accompany an
/// online index snapshot. Callers copy the set without knowing file meanings.
pub(crate) fn artifacts(data_dir: &Path) -> [PathBuf; 2] {
    [snapshot_path(data_dir), marker_path(data_dir)]
}

/// Accepts or refuses the configured schema against the index's durable state.
/// Any required marker write happens before this function returns, so callers
/// cannot create a versioned Tantivy index first.
pub(crate) fn accept(
    data_dir: &Path,
    current_schema: &str,
    parsed_current: &WayfinderSchema,
) -> Result<Accepted> {
    let snapshot = snapshot_path(data_dir);
    let has_snapshot = snapshot.exists();
    let previous = if has_snapshot {
        let raw = std::fs::read_to_string(&snapshot)
            .with_context(|| format!("reading stored schema {}", snapshot.display()))?;
        check_compatible(&raw, current_schema).with_context(|| {
            format!(
                "the index in {} was built with an incompatible schema",
                data_dir.display()
            )
        })?;
        Some(
            schema::parse(&raw)
                .context("parsing the index's stored schema for its analyzer contract")?,
        )
    } else {
        None
    };

    let facts = AnalysisFacts::combine(parsed_current, previous.as_ref());
    let marker = marker_path(data_dir);
    let persisted =
        if marker.exists() {
            Some(std::fs::read_to_string(&marker).with_context(|| {
                format!("reading analyzer contract marker {}", marker.display())
            })?)
        } else {
            None
        };

    let migration = decide(persisted.as_deref().map(str::trim), has_snapshot, facts);
    let marker_to_write = match migration {
        Migration::Accept => None,
        Migration::Advance(marker) => Some(marker),
        Migration::Adopt => {
            if has_snapshot && legacy_dynamic_text_has_indexed_terms(data_dir)? {
                bail!(
                    "the index in {} has legacy _dynamic_text postings; reindex into a fresh data directory for the current analyzer contract",
                    data_dir.display()
                );
            }
            Some(if facts.previous_has_dynamic_fields {
                CURRENT_LEGACY_DYNAMIC
            } else {
                CURRENT
            })
        }
        Migration::Refuse(reason) => bail!("the index in {} {reason}", data_dir.display()),
        Migration::Unsupported => bail!(
            "the index in {} has unsupported analyzer contract `{}`; reindex into a fresh data directory",
            data_dir.display(),
            persisted.as_deref().map(str::trim).unwrap_or_default()
        ),
    };

    if let Some(value) = marker_to_write {
        std::fs::write(&marker, value)
            .with_context(|| format!("writing analyzer contract marker {}", marker.display()))?;
    }

    Ok(Accepted {
        snapshot_path: snapshot,
        current_schema: current_schema.to_owned(),
    })
}

fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SCHEMA_SNAPSHOT_FILE)
}

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ANALYZER_MARKER_FILE)
}

#[derive(Clone, Copy, Debug, Default)]
struct AnalysisFacts {
    uses_static_text_length_bounded: bool,
    uses_changed_analyzed_path: bool,
    uses_analyzed_dynamic: bool,
    previous_has_dynamic_fields: bool,
}

impl AnalysisFacts {
    fn combine(current: &WayfinderSchema, previous: Option<&WayfinderSchema>) -> Self {
        Self {
            uses_static_text_length_bounded: current.uses_changed_static_text()
                || previous.is_some_and(WayfinderSchema::uses_changed_static_text),
            uses_changed_analyzed_path: current.uses_static_accent_folded_text()
                || current.uses_analyzed_dynamic_path()
                || previous.is_some_and(WayfinderSchema::uses_static_accent_folded_text)
                || previous.is_some_and(WayfinderSchema::uses_analyzed_dynamic_path),
            uses_analyzed_dynamic: current.uses_analyzed_dynamic_path()
                || previous.is_some_and(WayfinderSchema::uses_analyzed_dynamic_path),
            previous_has_dynamic_fields: previous.is_some_and(WayfinderSchema::has_dynamic_fields),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Migration {
    Accept,
    Advance(&'static str),
    Adopt,
    Refuse(&'static str),
    Unsupported,
}

fn decide(marker: Option<&str>, has_snapshot: bool, facts: AnalysisFacts) -> Migration {
    match marker {
        Some(CURRENT) => Migration::Accept,
        Some(V6) if facts.uses_static_text_length_bounded => Migration::Refuse(
            "uses the v6 static text analyzer contract; reindex into a fresh data directory for the current length bound",
        ),
        Some(V6) => Migration::Advance(CURRENT),
        Some(V6_LEGACY_DYNAMIC) if facts.uses_static_text_length_bounded => Migration::Refuse(
            "uses the v6 static text analyzer contract; reindex into a fresh data directory for the current length bound",
        ),
        Some(V6_LEGACY_DYNAMIC) if facts.uses_analyzed_dynamic => Migration::Refuse(
            "has a legacy _dynamic_text analyzer contract; reindex into a fresh data directory",
        ),
        Some(V6_LEGACY_DYNAMIC) => Migration::Advance(CURRENT_LEGACY_DYNAMIC),
        Some(V5) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses the pre-word-delimiter text analyzer contract; reindex into a fresh data directory",
        ),
        Some(V5) => Migration::Adopt,
        Some(V5_LEGACY_DYNAMIC) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses the pre-word-delimiter text analyzer contract; reindex into a fresh data directory",
        ),
        Some(V5_LEGACY_DYNAMIC) => Migration::Advance(CURRENT_LEGACY_DYNAMIC),
        Some(CURRENT_LEGACY_DYNAMIC) if facts.uses_static_text_length_bounded => Migration::Refuse(
            "uses a static text analyzer contract older than v7; reindex into a fresh data directory for the current length bound",
        ),
        Some(CURRENT_LEGACY_DYNAMIC) if facts.uses_analyzed_dynamic => Migration::Refuse(
            "predates the current UAX #29 text analyzer contract; reindex into a fresh data directory",
        ),
        Some(CURRENT_LEGACY_DYNAMIC) => Migration::Accept,
        Some(V4 | V3) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
        ),
        Some(V4 | V3) => Migration::Adopt,
        Some(V4_LEGACY_DYNAMIC | V3_LEGACY_DYNAMIC) if facts.uses_changed_analyzed_path => {
            Migration::Refuse(
                "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
            )
        }
        Some(V4_LEGACY_DYNAMIC | V3_LEGACY_DYNAMIC) => Migration::Advance(CURRENT_LEGACY_DYNAMIC),
        Some(V2) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
        ),
        Some(V2) => Migration::Adopt,
        Some(V2_LEGACY_DYNAMIC) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
        ),
        Some(V2_LEGACY_DYNAMIC) => Migration::Advance(CURRENT_LEGACY_DYNAMIC),
        Some(V1) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
        ),
        Some(V1) => Migration::Adopt,
        Some(V1_LEGACY_DYNAMIC) if facts.uses_changed_analyzed_path => Migration::Refuse(
            "predates the current text_en/text preset/dynamic-text analyzer contract; reindex into a fresh data directory",
        ),
        Some(V1_LEGACY_DYNAMIC) => Migration::Advance(CURRENT_LEGACY_DYNAMIC),
        Some(_) => Migration::Unsupported,
        None if has_snapshot && facts.uses_changed_analyzed_path => Migration::Refuse(
            "predates the current text_en/text preset/dynamic-text analyzer contract; reindex into a fresh data directory",
        ),
        None if has_snapshot => Migration::Adopt,
        None => Migration::Advance(CURRENT),
    }
}

fn check_compatible(previous: &str, current: &str) -> Result<()> {
    let previous = schema::compatibility_facts(previous, true)?;
    let current = schema::compatibility_facts(current, false)?;

    for old in &previous.fields {
        match current.fields.iter().find(|field| field.name == old.name) {
            None => bail!(
                "field `{}` was removed from the schema; the existing index still contains it; reindex into a fresh data directory",
                old.name
            ),
            Some(new) if new.type_ != old.type_ => bail!(
                "field `{}` changed type from `{}` to `{}`; the existing index was built with the old type; reindex into a fresh data directory",
                old.name,
                old.type_,
                new.type_
            ),
            Some(new)
                if (new.stored, new.fast, new.multi_valued)
                    != (old.stored, old.fast, old.multi_valued) =>
            {
                bail!(
                    "field `{}` changed options (stored/fast/multi_valued); the existing index was built with the old options; reindex into a fresh data directory",
                    old.name
                )
            }
            Some(_) => {}
        }
    }

    for new in &current.fields {
        if !previous.fields.iter().any(|field| field.name == new.name) {
            bail!(
                "field `{}` was added to the schema; Tantivy cannot add a field to an existing index; reindex into a fresh data directory",
                new.name
            );
        }
    }

    if previous.has_catch_all_fields != current.has_catch_all_fields {
        let detail = if previous.has_catch_all_fields {
            "the existing index still carries the catch-all fields they created"
        } else {
            "the existing index has no catch-all field to hold their values"
        };
        bail!(
            "[[dynamic_fields]] went from {} rule(s) to {}; {detail}; reindex into a fresh data directory",
            previous.dynamic_rule_count,
            current.dynamic_rule_count
        );
    }

    Ok(())
}

/// Checks raw term dictionaries, including deleted postings that have not yet
/// merged away. A schema snapshot alone cannot prove this catch-all was unused.
fn legacy_dynamic_text_has_indexed_terms(data_dir: &Path) -> Result<bool> {
    let index = Index::open_in_dir(data_dir).with_context(|| {
        format!(
            "opening legacy index in {} to verify its _dynamic_text postings",
            data_dir.display()
        )
    })?;
    let Ok(field) = index.schema().get_field(schema::DYNAMIC_TEXT_FIELD) else {
        return Ok(false);
    };
    let reader = index.reader().context("opening legacy index reader")?;
    for segment in reader.searcher().segment_readers() {
        if segment.inverted_index(field)?.terms().num_terms() != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_historical_marker_has_an_accept_advance_adopt_or_refuse_decision() {
        let raw = AnalysisFacts::default();
        let changed = AnalysisFacts {
            uses_static_text_length_bounded: true,
            uses_changed_analyzed_path: true,
            uses_analyzed_dynamic: true,
            previous_has_dynamic_fields: true,
        };
        let dynamic_changed = AnalysisFacts {
            uses_static_text_length_bounded: false,
            uses_changed_analyzed_path: true,
            uses_analyzed_dynamic: true,
            previous_has_dynamic_fields: true,
        };
        let cases = [
            (Some(CURRENT), raw, Migration::Accept),
            (Some(CURRENT_LEGACY_DYNAMIC), raw, Migration::Accept),
            (
                Some(CURRENT_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses a static text analyzer contract older than v7; reindex into a fresh data directory for the current length bound",
                ),
            ),
            (
                Some(CURRENT_LEGACY_DYNAMIC),
                dynamic_changed,
                Migration::Refuse(
                    "predates the current UAX #29 text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (Some(V6), raw, Migration::Advance(CURRENT)),
            (Some(V6), dynamic_changed, Migration::Advance(CURRENT)),
            (
                Some(V6),
                changed,
                Migration::Refuse(
                    "uses the v6 static text analyzer contract; reindex into a fresh data directory for the current length bound",
                ),
            ),
            (
                Some(V6_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V6_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses the v6 static text analyzer contract; reindex into a fresh data directory for the current length bound",
                ),
            ),
            (
                Some(V6_LEGACY_DYNAMIC),
                dynamic_changed,
                Migration::Refuse(
                    "has a legacy _dynamic_text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (Some(V5), raw, Migration::Adopt),
            (
                Some(V5),
                dynamic_changed,
                Migration::Refuse(
                    "uses the pre-word-delimiter text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (
                Some(V5),
                changed,
                Migration::Refuse(
                    "uses the pre-word-delimiter text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (
                Some(V5_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V5_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses the pre-word-delimiter text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (Some(V4), raw, Migration::Adopt),
            (
                Some(V4),
                changed,
                Migration::Refuse(
                    "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (
                Some(V4_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V4_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (Some(V3), raw, Migration::Adopt),
            (
                Some(V3),
                changed,
                Migration::Refuse(
                    "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (
                Some(V3_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V3_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses the pre-UAX #29 text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (Some(V2), raw, Migration::Adopt),
            (
                Some(V2),
                changed,
                Migration::Refuse(
                    "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
                ),
            ),
            (
                Some(V2_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V2_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
                ),
            ),
            (Some(V1), raw, Migration::Adopt),
            (
                Some(V1),
                changed,
                Migration::Refuse(
                    "uses a pre-folding text_en/text preset analyzer contract; reindex into a fresh data directory for accent folding",
                ),
            ),
            (
                Some(V1_LEGACY_DYNAMIC),
                raw,
                Migration::Advance(CURRENT_LEGACY_DYNAMIC),
            ),
            (
                Some(V1_LEGACY_DYNAMIC),
                changed,
                Migration::Refuse(
                    "predates the current text_en/text preset/dynamic-text analyzer contract; reindex into a fresh data directory",
                ),
            ),
            (None, raw, Migration::Adopt),
        ];
        for (marker, facts, expected) in cases {
            assert_eq!(decide(marker, true, facts), expected, "marker {marker:?}");
        }
        assert_eq!(decide(None, false, raw), Migration::Advance(CURRENT));
    }

    #[test]
    fn unknown_markers_fail_closed() {
        assert!(matches!(
            decide(Some("future"), true, AnalysisFacts::default()),
            Migration::Unsupported
        ));
    }

    #[test]
    fn stored_schema_comparison_is_table_driven() {
        const BASE: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[dynamic_fields]]
pattern = "*_s"
type = "string"
"#;
        let cases = [
            ("identical", BASE.to_owned(), None),
            (
                "validation-only required change",
                BASE.replace("required = true", "required = false"),
                None,
            ),
            (
                "removed field",
                BASE.replace(
                    "[[fields]]\nname = \"id\"\ntype = \"string\"\nstored = true\nrequired = true\n\n",
                    "",
                ),
                Some("removed"),
            ),
            (
                "changed type",
                BASE.replace("type = \"string\"", "type = \"int\""),
                Some("changed type"),
            ),
            (
                "changed option",
                BASE.replace("stored = true", "stored = false"),
                Some("changed options"),
            ),
            (
                "added field",
                format!(
                    "{BASE}\n[[fields]]\nname = \"extra\"\ntype = \"string\"\n"
                ),
                Some("added"),
            ),
            (
                "additional dynamic rule",
                format!(
                    "{BASE}\n[[dynamic_fields]]\npattern = \"*_i\"\ntype = \"int\"\n"
                ),
                None,
            ),
        ];

        for (name, current, expected_error) in cases {
            let result = check_compatible(BASE, &current);
            match expected_error {
                Some(fragment) => assert!(
                    format!("{:#}", result.expect_err(name)).contains(fragment),
                    "case {name}"
                ),
                None => result.unwrap_or_else(|error| panic!("case {name}: {error:#}")),
            }
        }
    }
}
