//! Colliding facet response labels (issue #149; finding 102).
//!
//! Solr writes duplicate JSON object members for two `facet.field` values that
//! use the same `{!key=x}` label. Wayfinder deliberately refuses that response
//! shape with a 400 rather than silently choosing one bucket. In contrast,
//! duplicate identical `facet.query` values are already coalesced by Solr and
//! remain a normal, one-member response.
//!
//! These fixtures cannot use `common::fixture` for their field-collision
//! evidence: parsing into `serde_json::Value` retains only the last `"x"`
//! member, so a last-write-wins implementation would falsely compare equal.

mod common;

use std::path::Path;

use axum::http::StatusCode;
use serde_json::{Value, json};

use common::diff::{load_manifest, load_manifest_errors};
use common::key_order::KeyOrder;
use common::{assert_matches_fixture, get, indexed_app};

const PRD: &str = include_str!("../docs/PRD.md");
const FIELD_FLAT: &str = include_str!("../solr-ref/responses/facet_collision_field_flat.json");
const FIELD_MAP: &str = include_str!("../solr-ref/responses/facet_collision_field_map.json");
const COLLISION_FIXTURES: [&str; 4] = [
    "facet_collision_field_flat",
    "facet_collision_field_map",
    "facet_collision_query_flat",
    "facet_collision_query_map",
];

/// Finding 102's ground truth must stay raw: each field fixture has two
/// literal outer `"x"` members, with category values first and id values
/// second. `serde_json::Value` keeps only the latter, demonstrating why a
/// parsed-fixture differential assertion would falsely green last-write-wins.
#[test]
fn field_collision_fixtures_preserve_duplicate_outer_members_in_raw_text() {
    for (shape, raw, expected_fields) in [
        (
            "flat",
            FIELD_FLAT,
            "\"facet_fields\":{\"x\":[\"animals\",2,\"classic\",2,\"garden\",1,\"misc\",1],\"x\":[\"doc1\",1,\"doc2\",1,\"doc3\",1,\"doc4\",1,\"doc5\",1]},\"facet_ranges\":",
        ),
        (
            "map",
            FIELD_MAP,
            "\"facet_fields\":{\"x\":{\"animals\":2,\"classic\":2,\"garden\":1,\"misc\":1},\"x\":{\"doc1\":1,\"doc2\":1,\"doc3\":1,\"doc4\":1,\"doc5\":1}},\"facet_ranges\":",
        ),
    ] {
        let structure = KeyOrder::parse(raw);
        assert_eq!(
            structure.keys_at("facet_counts.facet_fields", shape),
            ["x", "x"],
            "{shape} fixture must contain exactly two outer `x` members"
        );

        let compact: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        assert!(
            compact.contains(expected_fields),
            "{shape} fixture must put the exact category buckets under the first outer x and \
             id buckets under the second, bounded by facet_fields: {compact}"
        );

        let parsed: Value = serde_json::from_str(raw).expect("collision fixture must be JSON");
        let collapsed = parsed
            .pointer("/facet_counts/facet_fields/x")
            .unwrap_or_else(|| panic!("parsed {shape} fixture lost x entirely"));
        assert!(
            collapsed.to_string().contains("doc1") && !collapsed.to_string().contains("animals"),
            "serde_json::Value must retain only the final id bucket, proving parsed fixture \
             comparison would falsely accept last-write-wins: {collapsed}"
        );
    }
}

/// The four issue-149 captures intentionally stay outside the parsed-JSON
/// differential manifest. Its normaliser would discard the field fixture's
/// first duplicate member and falsely accept the current last-write-wins path.
#[test]
fn collision_fixtures_are_excluded_from_the_parsed_json_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = load_manifest(&root.join("solr-ref/manifest.tsv"));
    let error_manifest = load_manifest_errors(&root.join("solr-ref/manifest-errors.tsv"));
    assert!(
        manifest.iter().any(|entry| entry.name == "facet_basic"),
        "manifest sanity: a known ordinary facet row must remain so this exclusion check cannot \
         pass against an emptied manifest"
    );
    for fixture in COLLISION_FIXTURES {
        assert!(
            !manifest.iter().any(|entry| entry.name == fixture)
                && !error_manifest.iter().any(|entry| entry.name == fixture),
            "{fixture} must not enter a parsed JSON manifest until its parsed-Value \
             false-positive hazard is removed"
        );
    }
    assert!(
        PRD.contains("Colliding `facet.field` response labels are a hard 400")
            && PRD.contains("facet_collision_field_flat.json")
            && PRD.contains("facet_collision_field_map.json"),
        "the deliberate 400 divergence must remain ratified with both field fixtures"
    );
}

async fn assert_field_label_collision_is_rejected(path: &str) {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(&app, path).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "colliding facet.field response labels must be rejected, not silently last-write-wins; \
         got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400)
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        Some(400),
        "responseHeader.status must mirror the HTTP status, got {body}"
    );
    let message = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("collision rejection must explain itself, got {body}"));
    assert!(
        message.to_ascii_lowercase().contains("collid") && message.contains('x'),
        "collision error must identify the colliding label x, got: {message}"
    );
    assert!(
        body.get("facet_counts").is_none(),
        "a rejected collision must not emit a silently collapsed facet_counts block, got {body}"
    );
}

#[tokio::test]
async fn colliding_facet_field_labels_are_400_in_flat_mode() {
    assert_field_label_collision_is_rejected(
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dx%7Dcategory&facet.field=%7B%21key%3Dx%7Did&wt=json",
    )
    .await;
}

#[tokio::test]
async fn colliding_facet_field_labels_are_400_in_json_nl_map_mode() {
    assert_field_label_collision_is_rejected(
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dx%7Dcategory&facet.field=%7B%21key%3Dx%7Did\
         &json.nl=map&wt=json",
    )
    .await;
}

/// Solr itself coalesces the two identical `facet.query` values into one
/// member, irrespective of `json.nl`; Wayfinder must preserve that compatible
/// non-collision behaviour while rejecting only duplicate field labels.
#[tokio::test]
async fn duplicate_identical_facet_queries_match_the_flat_and_map_fixtures() {
    let (app, _dir) = indexed_app().await;
    for (fixture, path) in [
        (
            "facet_collision_query_flat",
            "select?q=*:*&rows=0&facet=true\
             &facet.query=category:animals&facet.query=category:animals&wt=json",
        ),
        (
            "facet_collision_query_map",
            "select?q=*:*&rows=0&facet=true\
             &facet.query=category:animals&facet.query=category:animals&json.nl=map&wt=json",
        ),
    ] {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{fixture}: got {body}");
        let queries = body
            .pointer("/facet_counts/facet_queries")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{fixture}: facet_queries must be an object, got {body}"));
        assert_eq!(
            queries,
            &serde_json::Map::from_iter([(String::from("category:animals"), json!(2))]),
            "{fixture}: duplicate identical facet.query values must coalesce to one member"
        );
        assert_matches_fixture(body, fixture);
    }
}
