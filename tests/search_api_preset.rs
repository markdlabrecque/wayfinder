//! Issue #58: `presets/search-api.toml` is a hand-authored schema that
//! expresses the Drupal `search_api_solr` module's field-naming convention as
//! Wayfinder `[[dynamic_fields]]` + `[[fields]]` rules, so any Drupal site
//! works against Wayfinder with zero per-site schema authoring.
//!
//! Ground truth for the naming convention: the captured module configset at
//! `solr-ref/search-api/configset/schema.xml`. The prefix -> Wayfinder-type
//! table this file tests against is reproduced in the issue #58 spec and in
//! comments below; nothing here is derived from what the preset happens to
//! contain.
//!
//! Note: issue #66 (already landed, in this branch's history) made
//! `check_sort` (`src/lib.rs`) and `check_facetable` (`src/facet.rs`) resolve
//! a field name via `WayfinderSchema::resolved_fast`, which correctly
//! consults `[[dynamic_fields]]` matches (not just statically declared
//! `[[fields]]`). The facet/sort round-trip assertions below now exercise
//! that dynamic-field-aware resolution directly against this preset.
//!
//! Two premises in the original version of this spec were also corrected
//! against the real captured Drupal<->Solr traffic in
//! `solr-ref/search-api/trace/*.json` and the module's own
//! `solr-ref/search-api/configset/schema_extra_fields.xml` /
//! `schema.xml`:
//!
//! 1. **`stored`**: Solr 7+ defaults `useDocValuesAsStored=true`, so a field
//!    with `docValues="true"` (the case for every dynamic prefix and every
//!    static field in the module's schema, `stored="false"` in schema.xml
//!    notwithstanding) is still returned on a plain `fl=*` query, *unless* it
//!    explicitly opts out with `useDocValuesAsStored="false"`. Only
//!    `sort_*`/`sort_X3b_en_*`/`sort_X3b_und_*` opt out that way; every other
//!    field in this preset is effectively stored. `trace/00010.json` (a real
//!    `fl=*,score` query) confirms this directly: the returned docs contain
//!    `site`, `index_id`, `hash`, `timestamp`, `boost_document`, `ss_type`,
//!    `sm_field_keywords`, `ds_created`, `its_field_rating`, `bs_sticky`, etc.
//!    — everything except `sort_*` fields.
//! 2. **`fast`**: the six static fields (`id`, `index_id`, `hash`, `site`,
//!    `timestamp`, `boost_document`) all carry `docValues="true"` in
//!    schema.xml, same as the dynamic prefixes that already got `fast = true`
//!    — so they must be `fast = true` too.

mod common;

use std::path::Path;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use wayfinder::schema::{self, WayfinderSchema};

fn preset_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/search-api.toml")
}

fn preset_toml() -> String {
    std::fs::read_to_string(preset_path()).unwrap_or_else(|e| {
        panic!(
            "presets/search-api.toml must exist and be readable: {e} (path: {})",
            preset_path().display()
        )
    })
}

fn load_preset() -> WayfinderSchema {
    schema::load(&preset_path()).unwrap_or_else(|e| {
        panic!("presets/search-api.toml must load as a valid Wayfinder schema: {e:#}")
    })
}

// --- 1. load test ------------------------------------------------------------

#[test]
fn preset_file_loads() {
    load_preset();
}

// --- 2. static field acceptance ----------------------------------------------

struct StaticExpectation {
    name: &'static str,
    type_: &'static str,
    stored: bool,
    required: bool,
    fast: bool,
}

const STATIC_FIELDS: &[StaticExpectation] = &[
    StaticExpectation {
        name: "id",
        type_: "string",
        stored: true,
        required: true,
        fast: true,
    },
    StaticExpectation {
        name: "index_id",
        type_: "string",
        stored: true,
        required: false,
        fast: true,
    },
    StaticExpectation {
        name: "hash",
        type_: "string",
        stored: true,
        required: false,
        fast: true,
    },
    StaticExpectation {
        name: "site",
        type_: "string",
        stored: true,
        required: false,
        fast: true,
    },
    StaticExpectation {
        name: "timestamp",
        type_: "date",
        stored: true,
        required: false,
        fast: true,
    },
    StaticExpectation {
        name: "boost_document",
        type_: "float",
        stored: true,
        required: false,
        fast: true,
    },
    // issue #300: solr_text_suggester sink field.
    StaticExpectation {
        name: "twm_suggest",
        type_: "text_general",
        stored: true,
        required: false,
        fast: false,
    },
];

#[test]
fn static_fields_match_the_search_api_solr_contract() {
    let wf = load_preset();
    for exp in STATIC_FIELDS {
        assert!(
            wf.field(exp.name).is_some(),
            "static field `{}` must have a Tantivy field handle",
            exp.name
        );
        let cfg = wf.field_config(exp.name).unwrap_or_else(|| {
            panic!("static field `{}` must be declared in [[fields]]", exp.name)
        });
        assert_eq!(
            cfg.type_, exp.type_,
            "field `{}` must be typed `{}`, got `{}`",
            exp.name, exp.type_, cfg.type_
        );
        assert_eq!(
            cfg.stored, exp.stored,
            "field `{}` stored must be {}",
            exp.name, exp.stored
        );
        assert_eq!(
            cfg.required, exp.required,
            "field `{}` required must be {}",
            exp.name, exp.required
        );
        assert_eq!(
            cfg.fast, exp.fast,
            "field `{}` fast must be {}",
            exp.name, exp.fast
        );
    }
}

#[test]
fn core_unique_key_and_name_are_set_for_the_shared_test_helpers() {
    let wf = load_preset();
    // `tests/common/mod.rs`'s helpers (`app_with_schema`, `post_docs`, `get`)
    // hardcode the core name "content", so any schema used with them must
    // match it.
    assert_eq!(wf.core.name, "content");
    assert_eq!(wf.core.unique_key, "id");
    // `default_field` just needs to be a valid field name; the module always
    // sends explicit qf/df per query, so no particular value is asserted.
    assert!(
        !wf.core.default_field.is_empty(),
        "core.default_field must be set to something (module always sends its own qf/df)"
    );
}

// --- 3. dynamic pattern matching + type resolution ---------------------------

struct DynamicExpectation {
    name: &'static str,
    type_: &'static str,
    multi_valued: bool,
    stored: bool,
    fast: bool,
}

/// One representative field name per prefix class in the issue #58 spec
/// table, with the exact type/cardinality/storage the table names.
const DYNAMIC_FIELDS: &[DynamicExpectation] = &[
    DynamicExpectation {
        name: "ss_search_api_language",
        type_: "string",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "sm_context_tags",
        type_: "string",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "ts_summary",
        type_: "text_general",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tm_summary",
        type_: "text_general",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "ts_X3b_en_title",
        type_: "text_en",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tm_X3b_en_body",
        type_: "text_en",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "is_weight",
        type_: "int",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "im_terms",
        type_: "int",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "its_created_int",
        type_: "long",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "itm_terms_int",
        type_: "long",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "fs_score",
        type_: "float",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "fm_scores",
        type_: "float",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "ps_precise",
        type_: "double",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "pm_precises",
        type_: "double",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "ds_created",
        type_: "date",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "dm_dates",
        type_: "date",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    // #341: DateRangeField-style interval type. The captured configset
    // (`solr-ref/search-api/configset/schema.xml:199-200`) declares
    // `drs_*`/`drm_*` as `indexed="true" stored="true"`, with NO `docValues`
    // -- unlike every other dynamic prefix above, so `fast` must be `false`
    // here.
    DynamicExpectation {
        name: "drs_created",
        type_: "date_range",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "drm_created",
        type_: "date_range",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "bs_status",
        // Wayfinder has no boolean type (`ResolvedType` in `src/schema.rs`
        // has no `Bool` variant); the nearest representable type is `string`,
        // storing `"true"`/`"false"` literally. Documented divergence, not a
        // bug — see the module-level comment.
        type_: "string",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "bm_flags",
        type_: "string",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "spellcheck_suggestions",
        type_: "text_general",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    // --- issue #300: search_api_solr non-default data types -----------------
    // solr_string_storage: string, stored, NOT fast (Solr's zs_/zm_ have no
    // docValues).
    DynamicExpectation {
        name: "zs_notes",
        type_: "string",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "zm_notes",
        type_: "string",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    // solr_string_docvalues: string, stored, fast (Solr's zdvs_/zdvm_ have
    // docValues=true).
    DynamicExpectation {
        name: "zdvs_uuid",
        type_: "string",
        multi_valued: false,
        stored: true,
        fast: true,
    },
    DynamicExpectation {
        name: "zdvm_uuid",
        type_: "string",
        multi_valued: true,
        stored: true,
        fast: true,
    },
    // solr_text_unstemmed / solr_text_omit_norms / solr_text_wstoken all map to
    // text_general (their Solr analyzer distinctions are a documented
    // scoring-quality divergence, not a type-level difference Wayfinder can
    // express).
    DynamicExpectation {
        name: "tus_unstemmed",
        type_: "text_general",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tum_unstemmed",
        type_: "text_general",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tos_omitnorms",
        type_: "text_general",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tom_omitnorms",
        type_: "text_general",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "tws_wstoken",
        type_: "text_general",
        multi_valued: false,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "twm_wstoken",
        type_: "text_general",
        multi_valued: true,
        stored: true,
        fast: false,
    },
    DynamicExpectation {
        name: "sort_title",
        type_: "string",
        multi_valued: false,
        stored: false,
        fast: true,
    },
];

#[test]
fn dynamic_patterns_resolve_every_prefix_class() {
    let wf = load_preset();
    for exp in DYNAMIC_FIELDS {
        assert!(
            !wf.is_static(exp.name),
            "`{}` must be reached via a dynamic pattern, not a static [[fields]] declaration",
            exp.name
        );
        let rule = wf.match_dynamic(exp.name).unwrap_or_else(|| {
            panic!(
                "`{}` must match a [[dynamic_fields]] pattern in the preset",
                exp.name
            )
        });
        assert_eq!(
            rule.type_, exp.type_,
            "`{}` must resolve to type `{}`, got `{}` (pattern `{}`)",
            exp.name, exp.type_, rule.type_, rule.pattern
        );
        assert_eq!(
            rule.multi_valued, exp.multi_valued,
            "`{}` (pattern `{}`) multi_valued must be {}",
            exp.name, rule.pattern, exp.multi_valued
        );
        assert_eq!(
            rule.stored, exp.stored,
            "`{}` (pattern `{}`) stored must be {}",
            exp.name, rule.pattern, exp.stored
        );
        assert_eq!(
            rule.fast, exp.fast,
            "`{}` (pattern `{}`) fast must be {}",
            exp.name, rule.pattern, exp.fast
        );
    }
}

// --- 4. round-trip: build one app, index one all-classes doc ----------------

/// One document carrying a representative value for every prefix class in the
/// spec table, plus every static field. Values are deliberately simple single
/// words (or ranges for dates) so query assertions don't depend on an
/// unverified assumption about exactly how the stemmer normalises a given
/// English word (`tests/schema_layer.rs` already covers stemming behaviour
/// itself) — the point here is only that indexing/analysis/storage round-trip
/// through the *preset's* declared types, not the tokenizer's own rules.
fn representative_doc() -> Value {
    json!([{
        "id": "doc1",
        "index_id": "test_index",
        "hash": "abc123",
        "site": "site1",
        "timestamp": "2026-07-28T12:00:00Z",
        "boost_document": 1.5,

        "ss_search_api_language": "en",
        "sm_context_tags": ["news", "featured"],

        "ts_summary": "Zerbinetta Appears",
        "tm_summary": ["Alpha Context", "Beta Context"],

        "ts_X3b_en_title": "Zanzibar Rising",
        "tm_X3b_en_body": ["Wombat Forest", "Gecko Canyon"],

        "is_weight": 5,
        "im_terms": [1, 2, 3],
        "its_created_int": 1000,
        "itm_terms_int": [10, 20],
        "fs_score": 1.5,
        "fm_scores": [1.1, 2.2],
        "ps_precise": 3.5,
        "pm_precises": [1.25, 2.75],
        "ds_created": "2026-07-28T12:00:00Z",
        "dm_dates": ["2026-07-28T12:00:00Z", "2026-07-29T00:00:00Z"],

        "bs_status": "true",
        "bm_flags": ["true", "false"],

        // --- issue #300: search_api_solr non-default data types ----------
        "zs_notes": "private note",
        "zm_notes": ["note one", "note two"],
        "zdvs_uuid": "abc-123",
        "zdvm_uuid": ["u1", "u2"],
        "tus_unstemmed": "Unstemmed Title",
        "tum_unstemmed": ["Alpha Unstemmed", "Beta Unstemmed"],
        "tos_omitnorms": "Omitnorms Title",
        "tom_omitnorms": ["Alpha Norms", "Beta Norms"],
        "tws_wstoken": "Whitespace Body",
        "twm_wstoken": ["Alpha Ws", "Beta Ws"],
        "twm_suggest": ["Suggestion One", "Suggestion Two"],

        "spellcheck_suggestions": ["Nautical Term"],
        "sort_title": "alpha"
    }])
}

async fn preset_app_with_doc() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), &preset_toml())
        .expect("presets/search-api.toml must build a working app");
    let (status, resp) = common::post_docs(&app, &representative_doc()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the all-classes representative doc must index cleanly: {resp}"
    );
    (app, dir)
}

/// `facet_counts.facet_fields.<field>` as a flat alternating array, the same
/// shape `tests/faceting.rs` uses.
fn flat_facet(body: &Value, field: &str) -> Vec<Value> {
    body.pointer(&format!("/facet_counts/facet_fields/{field}"))
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!("facet_counts.facet_fields.{field} must be a flat array, got: {body}")
        })
        .clone()
}

// -- query hit, one per prefix class ------------------------------------------

#[tokio::test]
async fn ss_field_is_queryable_by_exact_term() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=ss_search_api_language:en&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn sm_field_is_queryable_by_any_one_value() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=sm_context_tags:news&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn ts_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=ts_summary:zerbinetta&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tm_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tm_summary:alpha&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn ts_x3b_en_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=ts_X3b_en_title:zanzibar&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tm_x3b_en_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tm_X3b_en_body:wombat&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn is_field_is_queryable_by_exact_int() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=is_weight:5&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn im_field_is_queryable_by_any_one_value() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=im_terms:2&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn its_field_is_queryable_by_exact_long() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=its_created_int:1000&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn itm_field_is_queryable_by_any_one_value() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=itm_terms_int:20&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn fs_field_is_queryable_by_exact_float() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=fs_score:1.5&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn fm_field_is_queryable_by_any_one_value() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=fm_scores:2.2&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn ps_field_is_queryable_by_exact_double() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=ps_precise:3.5&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn pm_field_is_queryable_by_any_one_value() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=pm_precises:2.75&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn ds_field_is_queryable_by_date_range() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=ds_created:[2026-07-28T00:00:00Z+TO+2026-07-29T00:00:00Z]&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn dm_field_is_queryable_by_date_range() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=dm_dates:[2026-07-28T00:00:00Z+TO+2026-07-30T00:00:00Z]&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn bs_field_round_trips_boolean_as_string() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=bs_status:true&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn bm_field_round_trips_boolean_as_string() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=bm_flags:false&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

// -- issue #300: solr_string_storage / solr_string_docvalues round-trip -------

#[tokio::test]
async fn zs_field_round_trips_storage_only_string() {
    // solr_string_storage is storage-only in Solr (indexed=false); the
    // documented Wayfinder divergence is that it IS indexed here, so the
    // field is queryable as well as retrievable.
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=zs_notes:%22private+note%22&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn zm_field_round_trips_storage_only_string_multi() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=zm_notes:%22note+two%22&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn zdvs_field_round_trips_docvalues_string() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=zdvs_uuid:abc-123&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn zdvm_field_round_trips_docvalues_string_multi() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=zdvm_uuid:u2&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn zdvs_field_is_facetable() {
    // zdvs_* is fast=true (docValues), string-like, so facet.field works --
    // mirroring the ss_* facet test. This is the docValues-only type's one
    // practical advantage over storage-only in Solr, and Wayfinder preserves
    // it via fast=true.
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=zdvs_uuid&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let bucket = flat_facet(&resp, "zdvs_uuid");
    assert_eq!(bucket, vec![json!("abc-123"), json!(1)], "{resp}");
}

// -- issue #300: solr_text_* variants all map to text_general -----------------

#[tokio::test]
async fn tus_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tus_unstemmed:unstemmed&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tum_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tum_unstemmed:alpha&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tos_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tos_omitnorms:omitnorms&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tom_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tom_omitnorms:beta&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn tws_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=tws_wstoken:whitespace&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn twm_wstoken_field_is_full_text_queryable() {
    // Distinct from the twm_suggest static sink: this hits the twm_* dynamic
    // rule (solr_text_wstoken multi-valued), proving both the static and the
    // dynamic twm_ destinations resolve correctly.
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=twm_wstoken:alpha&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn twm_suggest_static_sink_is_full_text_queryable() {
    // solr_text_suggester indexes into the fixed static field twm_suggest
    // (static wins over the twm_* dynamic rule for this exact name). This is
    // the field the SuggestComponent (#291) reads.
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=twm_suggest:suggestion&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn spellcheck_field_is_full_text_queryable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) =
        common::get(&app, "select?q=spellcheck_suggestions:nautical&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

#[tokio::test]
async fn sort_field_is_queryable_by_exact_term() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=sort_title:alpha&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp.pointer("/response/numFound"),
        Some(&json!(1)),
        "{resp}"
    );
}

// -- facet: string-like classes only (ss_, sm_, bs_, bm_, sort_) --------------

#[tokio::test]
async fn ss_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=ss_search_api_language&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "`ss_*` is fast=true, string-like, so facet.field must work: {resp}"
    );
    let bucket = flat_facet(&resp, "ss_search_api_language");
    assert_eq!(bucket, vec![json!("en"), json!(1)], "{resp}");
}

#[tokio::test]
async fn sm_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=sm_context_tags&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let bucket = flat_facet(&resp, "sm_context_tags");
    assert!(
        bucket.contains(&json!("news")) && bucket.contains(&json!("featured")),
        "sm_context_tags facet must enumerate both values: {resp}"
    );
}

#[tokio::test]
async fn bs_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=bs_status&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let bucket = flat_facet(&resp, "bs_status");
    assert_eq!(bucket, vec![json!("true"), json!(1)], "{resp}");
}

#[tokio::test]
async fn bm_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=bm_flags&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let bucket = flat_facet(&resp, "bm_flags");
    assert!(
        bucket.contains(&json!("true")) && bucket.contains(&json!("false")),
        "bm_flags facet must enumerate both values: {resp}"
    );
}

#[tokio::test]
async fn site_static_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=site&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "`site` is a static field with docValues=true in schema.xml, so fast must be true \
         and facet.field must work: {resp}"
    );
    let bucket = flat_facet(&resp, "site");
    assert_eq!(bucket, vec![json!("site1"), json!(1)], "{resp}");
}

#[tokio::test]
async fn sort_field_is_facetable() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=sort_title&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let bucket = flat_facet(&resp, "sort_title");
    assert_eq!(bucket, vec![json!("alpha"), json!(1)], "{resp}");
}

// -- sort: fast=true numeric/date/sort_/ss_ classes (functional smoke test) --

#[tokio::test]
async fn fast_fields_are_sortable() {
    let (app, _dir) = preset_app_with_doc().await;
    for field in [
        "is_weight",
        "its_created_int",
        "fs_score",
        "ps_precise",
        "ds_created",
        "sort_title",
        "ss_search_api_language",
        "site",
        "timestamp",
    ] {
        let (status, resp) =
            common::get(&app, &format!("select?q=*:*&sort={field}+asc&wt=json")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "`{field}` is fast=true, so `sort={field} asc` must be a 200, got: {resp}"
        );
    }
}

// -- retrieval: stored=true classes come back via plain fl -------------------

#[tokio::test]
async fn stored_fields_are_returned_and_unstored_fields_are_not() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, resp) = common::get(&app, "select?q=id:doc1&wt=json").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let doc = resp
        .pointer("/response/docs/0")
        .unwrap_or_else(|| panic!("the doc must come back: {resp}"));

    // stored=true: id, ts_*, tm_*, ts_X3b_en_*, tm_X3b_en_*, spellcheck_*
    assert_eq!(doc.get("id"), Some(&json!("doc1")), "{doc}");
    assert_eq!(
        doc.get("ts_summary"),
        Some(&json!("Zerbinetta Appears")),
        "a stored ts_* field must round-trip its raw stored value: {doc}"
    );
    assert_eq!(
        doc.get("tm_summary"),
        Some(&json!(["Alpha Context", "Beta Context"])),
        "a stored tm_* field must round-trip as an array: {doc}"
    );
    assert_eq!(
        doc.get("ts_X3b_en_title"),
        Some(&json!("Zanzibar Rising")),
        "{doc}"
    );
    assert_eq!(
        doc.get("tm_X3b_en_body"),
        Some(&json!(["Wombat Forest", "Gecko Canyon"])),
        "{doc}"
    );
    assert_eq!(
        doc.get("spellcheck_suggestions"),
        Some(&json!(["Nautical Term"])),
        "{doc}"
    );

    // Solr 7+'s `useDocValuesAsStored=true` default means every field with
    // `docValues="true"` in the module's schema.xml is also returned on a
    // plain `fl=*` query, `stored="false"` in schema.xml notwithstanding.
    // `solr-ref/search-api/trace/00010.json` (a real `fl=*,score` capture)
    // confirms this directly. Only `sort_*` opts out via an explicit
    // `useDocValuesAsStored="false"` in `schema_extra_fields.xml`.
    assert_eq!(doc.get("index_id"), Some(&json!("test_index")), "{doc}");
    assert_eq!(doc.get("hash"), Some(&json!("abc123")), "{doc}");
    assert_eq!(doc.get("site"), Some(&json!("site1")), "{doc}");
    assert_eq!(
        doc.get("timestamp").and_then(Value::as_str),
        Some("2026-07-28T12:00:00Z"),
        "{doc}"
    );
    assert_eq!(doc.get("boost_document"), Some(&json!(1.5)), "{doc}");
    assert_eq!(
        doc.get("ss_search_api_language"),
        Some(&json!("en")),
        "{doc}"
    );
    assert_eq!(
        doc.get("sm_context_tags"),
        Some(&json!(["news", "featured"])),
        "{doc}"
    );
    assert_eq!(doc.get("is_weight"), Some(&json!(5)), "{doc}");
    assert_eq!(doc.get("im_terms"), Some(&json!([1, 2, 3])), "{doc}");
    assert_eq!(doc.get("its_created_int"), Some(&json!(1000)), "{doc}");
    assert_eq!(doc.get("itm_terms_int"), Some(&json!([10, 20])), "{doc}");
    assert_eq!(doc.get("fs_score"), Some(&json!(1.5)), "{doc}");
    assert_eq!(doc.get("fm_scores"), Some(&json!([1.1, 2.2])), "{doc}");
    assert_eq!(doc.get("ps_precise"), Some(&json!(3.5)), "{doc}");
    assert_eq!(doc.get("pm_precises"), Some(&json!([1.25, 2.75])), "{doc}");
    assert_eq!(
        doc.get("ds_created").and_then(Value::as_str),
        Some("2026-07-28T12:00:00Z"),
        "{doc}"
    );
    assert_eq!(
        doc.get("dm_dates")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>()),
        Some(vec!["2026-07-28T12:00:00Z", "2026-07-29T00:00:00Z"]),
        "{doc}"
    );
    assert_eq!(doc.get("bs_status"), Some(&json!("true")), "{doc}");
    assert_eq!(
        doc.get("bm_flags"),
        Some(&json!(["true", "false"])),
        "{doc}"
    );

    // issue #300: the solr_string_* and solr_text_* variant fields are all
    // stored=true, so they echo back via plain fl alongside the classes above.
    assert_eq!(doc.get("zs_notes"), Some(&json!("private note")), "{doc}");
    assert_eq!(
        doc.get("zm_notes"),
        Some(&json!(["note one", "note two"])),
        "{doc}"
    );
    assert_eq!(doc.get("zdvs_uuid"), Some(&json!("abc-123")), "{doc}");
    assert_eq!(doc.get("zdvm_uuid"), Some(&json!(["u1", "u2"])), "{doc}");
    assert_eq!(
        doc.get("tus_unstemmed"),
        Some(&json!("Unstemmed Title")),
        "{doc}"
    );
    assert_eq!(
        doc.get("tum_unstemmed"),
        Some(&json!(["Alpha Unstemmed", "Beta Unstemmed"])),
        "{doc}"
    );
    assert_eq!(
        doc.get("tos_omitnorms"),
        Some(&json!("Omitnorms Title")),
        "{doc}"
    );
    assert_eq!(
        doc.get("tom_omitnorms"),
        Some(&json!(["Alpha Norms", "Beta Norms"])),
        "{doc}"
    );
    assert_eq!(
        doc.get("tws_wstoken"),
        Some(&json!("Whitespace Body")),
        "{doc}"
    );
    assert_eq!(
        doc.get("twm_wstoken"),
        Some(&json!(["Alpha Ws", "Beta Ws"])),
        "{doc}"
    );
    assert_eq!(
        doc.get("twm_suggest"),
        Some(&json!(["Suggestion One", "Suggestion Two"])),
        "twm_suggest is a stored static field and must echo back its values: {doc}"
    );

    // stored=false: only `sort_*` opts out of `useDocValuesAsStored`, so it
    // alone must not be echoed back via plain fl, even though it indexed and
    // is queryable/facetable/sortable above.
    assert!(
        doc.get("sort_title").is_none(),
        "`sort_title` is stored=false (sort_* is the one useDocValuesAsStored=false \
         exception in schema_extra_fields.xml) and must not be echoed back: {doc}"
    );
}

// -- retrieval: `fl=*,score`, the request every Drupal search sends ------------

/// `fl=*,score` is what `search_api_solr` sends on **every** search: all 21
/// `/select` traces under `solr-ref/search-api/trace/` carry
/// `fl=%2A%2Cscore` verbatim and nothing else. This is therefore the
/// production-shaped end of issue #188's wildcard expansion, run against the
/// real preset rather than a purpose-built two-field schema.
///
/// What it pins is *key order*: `solr-ref/search-api/trace/00010.json`'s doc
/// keys end `..., "sm_field_keywords", "hash", "timestamp",
/// "ss_search_api_language", "score"` — `score` appended last, after every
/// dynamic field. `tests/select_fl_wildcard.rs`'s corpus-level tests mostly run
/// against schemas with at most one dynamic field; here the preset puts ~20
/// dynamic-rule fields in the doc, so an implementation that inserted `score`
/// between the declared `[[fields]]` and the dynamic ones (what #188 shipped
/// before review) lands it at position 7 of 26 and fails loudly.
#[tokio::test]
async fn preset_fl_star_plus_score_puts_score_last_after_every_dynamic_field() {
    let (app, _dir) = preset_app_with_doc().await;
    let (status, text) =
        common::key_order::get_text(&app, common::CORE, "select?q=id:doc1&fl=*,score&wt=json")
            .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let keys = common::key_order::KeyOrder::parse(&text)
        .keys_at("response.docs[0]", "preset fl=*,score response");

    // The preset's statically declared `[[fields]]`; everything else in the doc
    // came from a `[[dynamic_fields]]` rule.
    let declared: Vec<String> = load_preset()
        .fields
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let dynamic_keys: Vec<&String> = keys
        .iter()
        .filter(|k| *k != "score" && !declared.contains(k))
        .collect();
    assert!(
        dynamic_keys.len() > 1,
        "vacuity guard: this doc must carry several dynamic-rule fields, or `score` last and \
         `score` before the dynamic fields would be indistinguishable; got {keys:?}"
    );

    assert_eq!(
        keys.last().map(String::as_str),
        Some("score"),
        "`fl=*,score` must append `score` after every other key, dynamic fields included \
         (`solr-ref/search-api/trace/00010.json`), got: {text}"
    );
    let score_at = keys
        .iter()
        .position(|k| k == "score")
        .expect("`score` must be present at all under fl=*,score");
    assert_eq!(
        score_at,
        keys.len() - 1,
        "`score` must be the final key, not sit at index {score_at} of {}: {keys:?}",
        keys.len()
    );
}
