//! Solr `stats` component (issue #5, PRD §5): `stats=true` / `stats.field`
//! (repeatable) over numeric/date fast fields -> the `stats.stats_fields.<field>`
//! response block.
//!
//! Mirrors `crate::facet`'s structure and wiring in `src/lib.rs`'s `select`
//! handler: gated on `stats=true`, computed against the real base query (`q`
//! plus every `fq`), reusing `facet::BaseClauses`/`facet::narrowed` rather
//! than a second base-query-building pathway.
//!
//! Every expected value comes from a committed fixture in
//! `solr-ref/responses/stats_*.json`, captured against a dedicated `stats`
//! Solr core (`solr-ref/capture.sh`'s issue-#5 block) — see
//! `docs/solr-ref-findings.md` finding 51 for what that capture found,
//! including two shapes a naive implementation gets wrong:
//! - `min`/`max` render as JSON floats even for an integer field (Solr's
//!   stats component always computes in double precision).
//! - On zero matching docs, `mean` is the literal JSON *string* `"NaN"`, not
//!   `null` and not a bare `NaN` token (`serde_json` would render a native
//!   `f64::NAN` as `null`, which is a real, silent divergence).

use std::fmt;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use tantivy::query::{BooleanQuery, ExistsQuery, Occur};

use crate::core_index::CoreIndex;
use crate::facet::{self, BaseClauses};
use crate::params::Params;
use crate::schema::{VERSION_FIELD, ValueKind, WayfinderSchema};

/// Marks a `stats` error as one Solr raises *before* running the base query, so
/// its response envelope carries no `response` block — the exact same
/// distinction `facet::PreQueryFacetError` draws, and wired the same way in
/// `select`.
///
/// Only the `date_range` refusal is marked (#341, `dr341_err_stats`, which is
/// the sole captured stats-error fixture and has no `response` key). The three
/// generic refusals in `check_statable` stay unmarked: no fixture says which
/// side of the base query they fall on, so their existing behaviour is left
/// alone rather than changed on a guess.
#[derive(Debug)]
pub struct PreQueryStatsError(anyhow::Error);

impl fmt::Display for PreQueryStatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for PreQueryStatsError {}

/// Builds the whole `stats` block: `{"stats_fields": {...}}`, one entry per
/// `stats.field`. Returns `None` when `stats.field` was not given at all —
/// callers gate the whole `stats` key on `stats=true` separately, matching
/// `facet.field`'s own "the sub-param alone does not turn the feature on"
/// convention.
pub fn stats(index: &CoreIndex, params: &Params, base: &BaseClauses) -> Result<Value> {
    let fields = params.get_all("stats.field");
    if fields.is_empty() {
        return Ok(json!({ "stats_fields": {} }));
    }

    let base_query = BooleanQuery::from(
        base.iter()
            .map(|(occur, query)| (*occur, query.box_clone()))
            .collect::<BaseClauses>(),
    );

    let mut out = Map::new();
    for field_name in fields {
        check_statable(&index.wf_schema, field_name)?;

        let metrics = index.field_stats(field_name, &base_query)?;
        let has_value = ExistsQuery::new(field_name.to_string(), false);
        let missing = index.count(&facet::narrowed(base, Occur::MustNot, Box::new(has_value)))?;

        let count = metrics.count;
        let mean = if count == 0 {
            // Solr computes `mean = sum / count` in Java double arithmetic
            // (`0.0 / 0`), gets a real floating-point NaN, and its JSON writer
            // renders that as the quoted string `"NaN"` (bare `NaN` is not
            // valid JSON) — see finding 51 and `stats_zero.json`.
            json!("NaN")
        } else {
            json!(metrics.avg.unwrap_or(0.0))
        };

        out.insert(
            field_name.to_string(),
            json!({
                "min": metrics.min,
                "max": metrics.max,
                "count": count,
                "missing": missing,
                "sum": metrics.sum,
                "sumOfSquares": metrics.sum_of_squares.unwrap_or(0.0),
                "mean": mean,
                "stddev": metrics.std_deviation_sampling.unwrap_or(0.0),
            }),
        );
    }
    Ok(json!({ "stats_fields": Value::Object(out) }))
}

/// Refuses a `stats.field` Tantivy cannot aggregate on, the same way
/// `facet::check_facetable` refuses an unfacetable `facet.field` — no fixture
/// exercises this error path (issue #5's fixtures only cover `views`/`price`,
/// both fast numeric fields), so the message is not fixture-pinned, but a
/// clear 400 beats a panic or a silently empty result.
///
/// A **text** field has to be refused explicitly, not just an absent-`fast`
/// one: a fast (docValues) string field — e.g. `id` in both this issue's own
/// schema and `tests/stats.rs::STATS_SCHEMA_TOML` — passes the `fast` check
/// but is not a numeric/date column, so Tantivy's `ExtendedStats` aggregation
/// silently substitutes an empty column for it
/// (`fastfield/readers.rs`/`accessor_helpers.rs` in tantivy 0.26.1) rather
/// than erroring, which would otherwise come back as an honest-looking but
/// wrong `count: 0, missing: 0, min: null, max: null, mean: "NaN"` 200 for a
/// field that actually has a value on every doc — exactly the "silently
/// empty result" this whole check exists to prevent.
///
/// ponytail: same uniform-400-message simplification `facet::check_facetable`
/// already makes (undefined field / non-fast field / non-numeric field all
/// answer with Wayfinder's own error message rather than replicating Solr's
/// exact wording) — none of these three paths is fixture-pinned, unlike
/// `facet_non_docvalues_text`'s ratified divergence, so revisit if a stats
/// error fixture ever gets captured.
fn check_statable(schema: &WayfinderSchema, field_name: &str) -> Result<()> {
    // `_version_` is the sole internal exception. Keeping it here, alongside
    // the existing stats validation and aggregation call, avoids making it a
    // general schema-resolved field for sort, facet, or dynamic JSON paths.
    // This is a capability of the field, not the captured client's read path:
    // finding 132 (#293) shows the client reads `_version_` through a
    // `json.facet` aggregation, never the stats component.
    if field_name == VERSION_FIELD {
        return Ok(());
    }
    // #341/finding 186: `stats.field` on a `date_range` field is Solr's own
    // "not currently supported" refusal, naming the field TYPE rather than the
    // field. Checked before the three generic paths below because it must fire
    // identically for a declared static `date_range` (which is not `fast`, so it
    // would otherwise hit the docValues message) and for a dynamic one (which
    // has no `field_config` at all, so it would hit "undefined field").
    //
    // The Java class names inside the message are reproduced verbatim because
    // `dr341_err_stats` pins the whole string and finding 184 makes these `msg`
    // values part of the contract -- deliberately inconsistent with
    // `/schema/fieldtypes`, which honestly reports `wayfinder.DateRangeField`.
    if schema.resolved_value_kind(field_name) == Some(ValueKind::DateRange) {
        return Err(anyhow::Error::new(PreQueryStatsError(anyhow::anyhow!(
            "Field type date_range{{class=org.apache.solr.schema.DateRangeField,\
             analyzer=org.apache.solr.schema.FieldType$DefaultAnalyzer,\
             args={{class=solr.DateRangeField}}}} is not currently supported"
        ))));
    }
    match schema.field_config(field_name) {
        None => bail!("can not compute stats on undefined field: {field_name}"),
        Some(field) if !field.fast => {
            bail!("can not compute stats on a field w/o fast values (docValues): {field_name}")
        }
        Some(_) if schema.value_kind(field_name) == Some(ValueKind::Text) => {
            bail!(
                "can not compute stats on the text field `{field_name}`: stats needs a numeric or date field"
            )
        }
        Some(_) => Ok(()),
    }
}
